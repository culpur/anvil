/// Full-screen TUI for Anvil — ratatui-based alternate-screen layout.
///
/// Layout (top to bottom):
///   ┌─ header bar (1 line) ─────────────────────────────────────────┐
///   │ scrollable content area (messages, tool calls, results)       │
///   ├─ separator ─── (1 line) ──────────────────────────────────────┤
///   │ input line (1 line)                                           │
///   │ blank line                                                    │
///   │ status line 1: model · git · tokens                           │
///   │ status line 2: context bar · session %                        │
///   │ status line 3: permission mode · hints                        │
///   └───────────────────────────────────────────────────────────────┘
///
/// The TUI owns the terminal for its entire lifetime.  `Drop` restores the
/// terminal to the normal state so the shell is never left broken.
///
/// Output from the model is delivered via `TuiEvent` values pushed over an
/// `std::sync::mpsc` channel.  The caller (main.rs) passes a `TuiSender` to
/// `DefaultRuntimeClient` and `CliToolExecutor` so they can send events
/// instead of writing to stdout.
use std::io::{self, Stdout};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use crossterm::terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;

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
enum LogEntry {
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
}

impl LogEntry {
    /// Render this entry as a list of ratatui `Line`s for display.
    fn to_lines(&self, max_width: u16) -> Vec<Line<'static>> {
        let width = max_width.saturating_sub(4) as usize;
        match self {
            LogEntry::User(text) => {
                let mut lines = vec![Line::from(vec![
                    Span::styled("You  ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::styled(
                        text.lines().next().unwrap_or("").to_string(),
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                    ),
                ])];
                for extra in text.lines().skip(1) {
                    lines.push(Line::from(Span::styled(
                        format!("     {extra}"),
                        Style::default().fg(Color::White),
                    )));
                }
                lines.push(Line::from(""));
                lines
            }
            LogEntry::Assistant(text) => {
                // Strip ANSI escapes before passing to ratatui which handles
                // its own styling.
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
                    (Color::Red, "✗", format!("{name} (error)"))
                } else if *done {
                    (Color::Green, "✓", name.clone())
                } else {
                    (Color::Yellow, "●", name.clone())
                };

                let dash_count = (width.saturating_sub(label.len() + 6)).min(width);
                let dashes = "─".repeat(dash_count);
                let top = format!("╭─ {icon} {label} {dashes}╮");
                let bot = format!("╰{:─<width$}╯", "", width = width + 2);

                let mut lines = vec![Line::from(Span::styled(
                    top,
                    Style::default().fg(border_color),
                ))];

                // Detail lines — indent inside the box
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
                let mut lines = vec![Line::from(vec![
                    Span::styled(
                        "◆  ",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    ),
                    Span::styled(
                        text.lines().next().unwrap_or("").to_string(),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ])];
                for extra in text.lines().skip(1) {
                    lines.push(Line::from(Span::styled(
                        format!("   {extra}"),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
                lines.push(Line::from(""));
                lines
            }
        }
    }
}

// ─── Tab ──────────────────────────────────────────────────────────────────────

/// All per-tab mutable state.  Each tab is an independent conversation with
/// its own log, input buffer, scroll position, history, and token counters.
/// The model and session-id are also per-tab so they can diverge after the
/// initial creation.
struct Tab {
    id: usize,
    name: String,
    /// Message log.
    log: Vec<LogEntry>,
    /// Accumulates streaming assistant text until `TextDone`.
    pending_text: String,
    /// Scrolling offset (lines from top).
    scroll: usize,
    /// Input buffer.
    input: String,
    /// Cursor position inside `input` (byte offset).
    cursor: usize,
    /// History of submitted inputs.
    history: Vec<String>,
    /// Current history navigation index.
    history_idx: Option<usize>,
    /// Backup of the live input when navigating history.
    history_backup: Option<String>,
    /// Current thinking label (empty string = not thinking).
    think_label: String,
    think_start: Option<Instant>,
    think_frame: usize,
    /// Cumulative token counts.
    input_tokens: u32,
    output_tokens: u32,
    /// When this tab was created.
    session_start: Instant,
    /// Human-readable model name for this tab.
    model: String,
    /// Session identifier for this tab.
    session_id: String,
    /// Slash-command completion popup state.
    completion: CompletionPopup,
    /// True if there are unread messages in this tab while it was not active.
    has_unread: bool,
}

impl Tab {
    fn new(id: usize, name: impl Into<String>, model: impl Into<String>, session_id: impl Into<String>) -> Self {
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
        }
    }
}

// ─── AnvilTui ─────────────────────────────────────────────────────────────────

/// The full-screen TUI driver.
///
/// Create with `AnvilTui::new()`, then call `run()` to enter the main loop.
/// The caller passes prompts back via the returned `String` from `run()`.
pub struct AnvilTui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// All open tabs.
    tabs: Vec<Tab>,
    /// Index into `tabs` of the currently visible tab.
    active_tab: usize,
    /// Channel receiver from the model/tool pipeline.
    rx: Receiver<TuiEvent>,
    /// True once /exit or Ctrl+D has been issued.
    exiting: bool,
    /// Current git branch name (empty if not in a git repo).
    git_branch: String,
    /// Compact diff stats string e.g. "+12,-5" (empty if no diff or not in git repo).
    git_diff_stats: String,
    /// Current permission mode display label.
    permission_mode: String,
    /// Maximum context window tokens for the current model.
    context_max_tokens: u32,
    /// Running counter for assigning tab IDs.
    next_tab_id: usize,
    /// QMD status line: docs indexed, vectors, last update
    qmd_status: String,
    /// Last archive info shown to user
    last_archive_status: String,
}

/// Tracks the state of the slash-command completion popup.
#[derive(Debug, Default)]
struct CompletionPopup {
    /// Whether the popup is visible.
    visible: bool,
    /// Filtered candidates matching the current input prefix.
    matches: Vec<CompletionItem>,
    /// Index of the highlighted item.
    selected: usize,
}

#[derive(Debug, Clone)]
struct CompletionItem {
    /// The text to insert (e.g. "/provider" or "anthropic").
    insert: String,
    /// Short description shown next to the command.
    hint: String,
}

const THINK_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

impl AnvilTui {
    /// Enter alternate screen and return the TUI + the sender for model events.
    pub fn new(
        model: impl Into<String>,
        session_id: impl Into<String>,
        permission_mode: impl Into<String>,
    ) -> io::Result<(Self, TuiSender)> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        crossterm::execute!(stdout, terminal::EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        let (tx, rx) = mpsc::sync_channel::<TuiEvent>(512);

        let model_str: String = model.into();
        let session_id_str: String = session_id.into();
        let context_max = context_max_for_model(&model_str);
        let (git_branch, git_diff_stats) = fetch_git_info();

        let initial_tab = Tab::new(1, "main", model_str.clone(), session_id_str);

        Ok((
            Self {
                terminal,
                tabs: vec![initial_tab],
                active_tab: 0,
                rx,
                exiting: false,
                git_branch,
                git_diff_stats,
                permission_mode: permission_mode.into(),
                context_max_tokens: context_max,
                next_tab_id: 2,
                qmd_status: String::new(),
                last_archive_status: String::new(),
            },
            TuiSender(tx),
        ))
    }

    // ─── Tab accessors ───────────────────────────────────────────────────────

    fn active_tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    /// Add a new tab.  Returns the (0-based) index of the new tab.
    pub fn new_tab(&mut self, name: impl Into<String>, model: impl Into<String>, session_id: impl Into<String>) -> usize {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let tab = Tab::new(id, name, model, session_id);
        self.tabs.push(tab);
        self.tabs.len() - 1
    }

    /// Switch to the tab at 0-based index.  Clears the unread marker on the target.
    pub fn switch_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active_tab = index;
            self.tabs[index].has_unread = false;
        }
    }

    /// Switch to the next tab (wraps around).
    fn next_tab(&mut self) {
        let next = (self.active_tab + 1) % self.tabs.len();
        self.switch_tab(next);
    }

    /// Switch to the previous tab (wraps around).
    fn prev_tab(&mut self) {
        let prev = if self.active_tab == 0 {
            self.tabs.len() - 1
        } else {
            self.active_tab - 1
        };
        self.switch_tab(prev);
    }

    /// Close the active tab.  The last tab cannot be closed.
    /// Returns the name of the closed tab, or None if there was only one tab.
    pub fn close_active_tab(&mut self) -> Option<String> {
        if self.tabs.len() <= 1 {
            return None;
        }
        let name = self.tabs[self.active_tab].name.clone();
        self.tabs.remove(self.active_tab);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        Some(name)
    }

    /// Rename the active tab.
    pub fn rename_active_tab(&mut self, name: impl Into<String>) {
        self.active_tab_mut().name = name.into();
    }

    /// Return the number of open tabs.
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Return a list of (index, id, name, has_unread) tuples.
    pub fn tab_list(&self) -> Vec<(usize, usize, &str, bool)> {
        self.tabs.iter().enumerate().map(|(i, t)| (i, t.id, t.name.as_str(), t.has_unread)).collect()
    }

    /// Return the 0-based index of the currently active tab.
    pub fn active_tab_index(&self) -> usize {
        self.active_tab
    }

    /// Update the model for the active tab and recalculate context limit.
    pub fn set_model(&mut self, model: impl Into<String>) {
        let model_str = model.into();
        self.context_max_tokens = context_max_for_model(&model_str);
        self.active_tab_mut().model = model_str;
    }

    /// Update the session-id for the active tab.
    pub fn set_session_id(&mut self, id: impl Into<String>) {
        self.active_tab_mut().session_id = id.into();
    }

    // ─── Public interface ────────────────────────────────────────────────────

    /// Draw the current state.
    fn draw(&mut self) -> io::Result<()> {
        // Snapshot per-tab data from the active tab.
        let tab = self.active_tab();
        let log_snapshot = tab.log.clone();
        let pending = tab.pending_text.clone();
        let think = tab.think_label.clone();
        let think_frame = THINK_FRAMES[tab.think_frame % THINK_FRAMES.len()];
        let input_text = tab.input.clone();
        let cursor_pos = tab.cursor;
        let scroll = tab.scroll;
        let model = tab.model.clone();
        let session_id = tab.session_id.clone();
        let input_tokens = tab.input_tokens;
        let output_tokens = tab.output_tokens;
        let elapsed = tab.session_start.elapsed();
        let completion_visible = tab.completion.visible;
        let completion_selected = tab.completion.selected;
        let completion_matches: Vec<(String, String)> = tab
            .completion
            .matches
            .iter()
            .map(|c| (c.insert.clone(), c.hint.clone()))
            .collect();
        // Snapshot tab bar state.
        let tab_infos: Vec<(usize, String, bool, bool)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id, t.name.clone(), i == self.active_tab, t.has_unread))
            .collect();
        let git_branch = self.git_branch.clone();
        let git_diff_stats = self.git_diff_stats.clone();
        let permission_mode = self.permission_mode.clone();
        let context_max_tokens = self.context_max_tokens;
        let qmd_status = self.qmd_status.clone();
        let last_archive_status = self.last_archive_status.clone();

        self.terminal.draw(|frame| {
            let size = frame.area();
            let width = size.width as usize;

            // ── layout ──────────────────────────────────────────────────────
            // header=2 (tab bar + model/session line), content=fill, footer=6
            // footer breakdown (rendered manually within the 6-line block):
            //   line 0: separator ─────
            //   line 1: ❯ input text
            //   line 2: blank
            //   line 3: model | git | tokens
            //   line 4: context bar | session%
            //   line 5: permission mode | hints
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Min(4),
                    Constraint::Length(6),
                ])
                .split(size);

            let header_area = chunks[0];
            let content_area = chunks[1];
            let footer_area = chunks[2];

            // Split header into two rows: tab bar (row 0) + model/session (row 1).
            let header_rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Length(1)])
                .split(header_area);
            let tab_bar_area = header_rows[0];
            let model_bar_area = header_rows[1];

            // ── tab bar (row 0) ──────────────────────────────────────────────
            // Render: [1: main*] [2: refactor] ...  (active tab bold cyan, inactive dim)
            let mut tab_spans: Vec<Span<'static>> = vec![Span::raw(" ")];
            for (tab_id, tab_name, is_active, has_unread) in &tab_infos {
                let label = if *has_unread {
                    format!("[{}: {}*]", tab_id, tab_name)
                } else {
                    format!("[{}: {}]", tab_id, tab_name)
                };
                let style = if *is_active {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(Color::Rgb(0x66, 0x66, 0x88))
                        .add_modifier(Modifier::DIM)
                };
                tab_spans.push(Span::styled(label, style));
                tab_spans.push(Span::raw(" "));
            }
            // Hint on the right side of the tab bar
            let hint = Span::styled(
                "Ctrl+T new  Ctrl+W close  Ctrl+←/→ switch",
                Style::default().fg(Color::Rgb(0x44, 0x44, 0x55)),
            );
            let tab_bar_left_len: usize = tab_spans.iter().map(|s| s.content.chars().count()).sum();
            let hint_len = hint.content.chars().count();
            let pad = width.saturating_sub(tab_bar_left_len + hint_len);
            tab_spans.push(Span::raw(" ".repeat(pad)));
            tab_spans.push(hint);
            let tab_bar_widget = Paragraph::new(Line::from(tab_spans))
                .style(Style::default().bg(Color::Rgb(0x12, 0x12, 0x22)));
            frame.render_widget(tab_bar_widget, tab_bar_area);

            // ── model/session bar (row 1) ────────────────────────────────────
            let short_session = if session_id.len() > 20 {
                format!("…{}", &session_id[session_id.len() - 18..])
            } else {
                session_id.clone()
            };
            let model_bar_text = format!(
                " ⚒ Anvil v{}  │  {}  │  {}",
                env!("CARGO_PKG_VERSION"),
                model,
                short_session
            );
            let model_bar = Paragraph::new(model_bar_text).style(
                Style::default()
                    .fg(Color::Cyan)
                    .bg(Color::Rgb(0x1a, 0x1a, 0x2e))
                    .add_modifier(Modifier::BOLD),
            );
            frame.render_widget(model_bar, model_bar_area);

            // ── content ─────────────────────────────────────────────────────
            let content_width = content_area.width;
            let mut all_lines: Vec<Line<'static>> = Vec::new();

            for entry in &log_snapshot {
                all_lines.extend(entry.to_lines(content_width));
            }

            // Streaming assistant text in progress
            if !pending.is_empty() {
                let clean = strip_ansi(&pending);
                all_lines.extend(
                    clean
                        .lines()
                        .map(|l| Line::from(Span::raw(l.to_string()))),
                );
            }

            // Thinking spinner
            if !think.is_empty() {
                let elapsed_think = format!("{:.1}s", {
                    // estimate from think_frame count (ticks ~4/s)
                    think_frame.len() as f64 * 0.25
                });
                all_lines.push(Line::from(vec![
                    Span::styled(
                        format!("{think_frame} "),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        think.clone(),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    ),
                    Span::styled(
                        format!("  ({})", elapsed_think),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }

            let total_lines = all_lines.len();
            let visible_height = content_area.height as usize;
            // Auto-scroll: keep the bottom visible when not manually scrolled
            let max_scroll = total_lines.saturating_sub(visible_height);
            let effective_scroll = scroll.min(max_scroll);

            let visible_lines: Vec<Line<'static>> = all_lines
                .into_iter()
                .skip(effective_scroll)
                .take(visible_height)
                .collect();

            let content_widget =
                Paragraph::new(Text::from(visible_lines)).style(Style::default().fg(Color::White));
            frame.render_widget(content_widget, content_area);

            // ── footer (6 lines) ─────────────────────────────────────────────
            // Build all 6 lines as a Paragraph rendered into footer_area.

            // Line 0: separator
            let separator = "─".repeat(width);
            let line0 = Line::from(Span::styled(
                separator,
                Style::default().fg(Color::Rgb(0x44, 0x44, 0x55)),
            ));

            // Line 1: input prompt + text
            // We use a block-cursor character (█) appended at the cursor position.
            let before_cursor = input_text
                .char_indices()
                .take_while(|(i, _)| *i < cursor_pos)
                .map(|(_, c)| c)
                .collect::<String>();
            let cursor_char = input_text[cursor_pos..]
                .chars()
                .next()
                .map(|_| {
                    input_text[cursor_pos..]
                        .chars()
                        .next()
                        .unwrap()
                        .to_string()
                })
                .unwrap_or_else(|| " ".to_string());
            let after_cursor = if cursor_pos < input_text.len() {
                let next = next_char_boundary(&input_text, cursor_pos);
                input_text[next..].to_string()
            } else {
                String::new()
            };
            let line1 = Line::from(vec![
                Span::styled(
                    "❯ ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(before_cursor, Style::default().fg(Color::White)),
                Span::styled(
                    cursor_char,
                    Style::default()
                        .fg(Color::Rgb(0x1a, 0x1a, 0x1a))
                        .bg(Color::White),
                ),
                Span::styled(after_cursor, Style::default().fg(Color::White)),
            ]);

            // Line 2: blank
            let line2 = Line::from("");

            // Line 3: Model: {name} | Total: {n}M | ⌐{branch} | (+x,-y)    {total} tokens
            let total_tokens = input_tokens.saturating_add(output_tokens);
            let total_m = total_tokens as f64 / 1_000_000.0;
            let right3 = format!("{total_tokens} tokens");
            let line3 = build_left_right_line(
                build_status1_spans(&model, total_m, &git_branch, &git_diff_stats),
                vec![Span::styled(
                    right3,
                    Style::default().fg(Color::Rgb(0x88, 0x88, 0x88)),
                )],
                width,
            );

            // Line 4: Context: [{bar}] {used}k/{max}k ({pct}%) | Session: {session%} | Block: {dur}    currentVersion: x.y.z
            let used_tokens = input_tokens; // context usage is primarily input
            let bar_width: usize = 16;
            let pct = if context_max_tokens > 0 {
                ((used_tokens as f64 / context_max_tokens as f64) * 100.0).min(100.0)
            } else {
                0.0
            };
            let filled = ((pct / 100.0) * bar_width as f64).round() as usize;
            let empty = bar_width.saturating_sub(filled);
            let bar_filled = "█".repeat(filled);
            let bar_empty = "░".repeat(empty);
            let used_k = used_tokens / 1000;
            let max_k = context_max_tokens / 1000;
            let session_pct = if context_max_tokens > 0 {
                pct
            } else {
                0.0
            };
            let secs = elapsed.as_secs();
            let dur_str = if secs < 3600 {
                format!("{}m", secs / 60)
            } else {
                format!("{}hr", secs / 3600)
            };
            let version_str = format!("currentVersion: {}", env!("CARGO_PKG_VERSION"));

            let line4 = build_left_right_line(
                vec![
                    Span::raw("Context: ["),
                    Span::styled(bar_filled, Style::default().fg(Color::Blue)),
                    Span::styled(bar_empty, Style::default().fg(Color::Rgb(0x33, 0x33, 0x33))),
                    Span::raw("] "),
                    Span::styled(
                        format!("{used_k}k/{max_k}k ({pct:.0}%)"),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        format!(" | Session: {session_pct:.1}% | Block: {dur_str}"),
                        Style::default().fg(Color::Rgb(0x88, 0x88, 0x88)),
                    ),
                ],
                vec![Span::styled(
                    version_str,
                    Style::default().fg(Color::Rgb(0x66, 0x66, 0x66)),
                )],
                width,
            );

            // Line 5: ▸▸ permissions | QMD status | archive status
            let perm_display = permission_mode_display(&permission_mode);
            let mut line5_left = vec![
                Span::styled(
                    "▸▸ ",
                    Style::default().fg(Color::Rgb(0x88, 0x88, 0x44)),
                ),
                Span::styled(
                    perm_display,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM),
                ),
            ];
            // QMD status
            if !qmd_status.is_empty() {
                line5_left.push(Span::styled(
                    format!("  │  📚 {qmd_status}"),
                    Style::default().fg(Color::Rgb(0x55, 0x88, 0x55)),
                ));
            }
            // Archive status
            if !last_archive_status.is_empty() {
                line5_left.push(Span::styled(
                    format!("  │  📦 {last_archive_status}"),
                    Style::default().fg(Color::Rgb(0x55, 0x77, 0xAA)),
                ));
            }
            let line5 = Line::from(line5_left);

            let footer_lines = vec![line0, line1, line2, line3, line4, line5];
            let footer_widget = Paragraph::new(Text::from(footer_lines));
            frame.render_widget(footer_widget, footer_area);

            // ─── Completion popup overlay ────────────────────────────────
            if completion_visible && !completion_matches.is_empty() {
                let popup_height = (completion_matches.len() as u16).min(12) + 2; // +2 for border
                let popup_width = (width as u16).min(60);
                // Position above the input line
                let popup_y = footer_area.y.saturating_sub(popup_height);
                let popup_area = ratatui::layout::Rect {
                    x: footer_area.x + 1,
                    y: popup_y,
                    width: popup_width,
                    height: popup_height,
                };

                let items: Vec<Line<'static>> = completion_matches
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        let is_selected = i == completion_selected;
                        let (fg, bg) = if is_selected {
                            (Color::Black, Color::Cyan)
                        } else {
                            (Color::White, Color::Rgb(0x1a, 0x1a, 0x2e))
                        };
                        let cmd_width = 24.min(popup_width as usize - 4);
                        let padded_cmd = format!("{:<width$}", item.0, width = cmd_width);
                        Line::from(vec![
                            Span::styled(
                                format!(" {padded_cmd}"),
                                Style::default().fg(fg).bg(bg),
                            ),
                            Span::styled(
                                format!(" {}", item.1),
                                Style::default()
                                    .fg(if is_selected { Color::Black } else { Color::Rgb(0x88, 0x88, 0x88) })
                                    .bg(bg)
                                    .add_modifier(Modifier::DIM),
                            ),
                        ])
                    })
                    .collect();

                let popup_widget = Paragraph::new(Text::from(items))
                    .block(
                        ratatui::widgets::Block::default()
                            .borders(ratatui::widgets::Borders::ALL)
                            .border_style(Style::default().fg(Color::Rgb(0x44, 0x44, 0x66)))
                            .style(Style::default().bg(Color::Rgb(0x1a, 0x1a, 0x2e))),
                    );
                frame.render_widget(ratatui::widgets::Clear, popup_area);
                frame.render_widget(popup_widget, popup_area);
            }

            // Position the terminal cursor on the input line (footer row 1).
            // The prompt "❯ " is 2 display columns wide.
            let prompt_width: u16 = 2; // "❯ "
            let input_char_count = input_text[..cursor_pos.min(input_text.len())]
                .chars()
                .count() as u16;
            let cursor_x = footer_area.x + prompt_width + input_char_count;
            let cursor_y = footer_area.y + 1; // row 1 of footer = input line
            let max_x = footer_area.x + footer_area.width.saturating_sub(1);
            frame.set_cursor_position(Position {
                x: cursor_x.min(max_x),
                y: cursor_y,
            });
        })?;
        Ok(())
    }

    /// Flush any pending streaming text into the log as a completed assistant message.
    fn flush_pending_text(&mut self) {
        let text = std::mem::take(&mut self.active_tab_mut().pending_text);
        if !text.trim().is_empty() {
            self.active_tab_mut().log.push(LogEntry::Assistant(text));
        }
    }

    /// Drain all queued TUI events without blocking.
    fn drain_events(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(event) => self.apply_tui_event(event),
                Err(_) => break,
            }
        }
    }

    fn apply_tui_event(&mut self, event: TuiEvent) {
        match event {
            TuiEvent::TextDelta(text) => {
                self.active_tab_mut().pending_text.push_str(&text);
            }
            TuiEvent::TextDone => {
                self.flush_pending_text();
                let tab = self.active_tab_mut();
                tab.think_label.clear();
                tab.think_start = None;
            }
            TuiEvent::ToolCallStart { name } => {
                self.flush_pending_text();
                self.active_tab_mut().log.push(LogEntry::ToolCall {
                    name,
                    detail: String::new(),
                    done: false,
                    is_error: false,
                });
            }
            TuiEvent::ToolCallActive { name, detail } => {
                // Update the most recent matching ToolCall entry
                for entry in self.active_tab_mut().log.iter_mut().rev() {
                    if let LogEntry::ToolCall {
                        name: n,
                        detail: d,
                        done,
                        ..
                    } = entry
                    {
                        if *n == name && !*done {
                            *d = detail;
                            break;
                        }
                    }
                }
            }
            TuiEvent::ToolResult {
                name,
                summary,
                is_error,
            } => {
                // Mark the matching pending tool call as done, then add a result
                for entry in self.active_tab_mut().log.iter_mut().rev() {
                    if let LogEntry::ToolCall {
                        name: n,
                        done,
                        is_error: err,
                        ..
                    } = entry
                    {
                        if *n == name && !*done {
                            *done = true;
                            *err = is_error;
                            break;
                        }
                    }
                }
                // Add a compact result line as a system entry
                let label = if is_error { "error" } else { "ok" };
                let first_line = summary
                    .lines()
                    .next()
                    .map(|l| truncate_str(l, 120))
                    .unwrap_or_default();
                if !first_line.is_empty() {
                    self.active_tab_mut().log.push(LogEntry::System(format!(
                        "{name} [{label}]: {first_line}"
                    )));
                }
            }
            TuiEvent::ThinkLabel(label) => {
                let tab = self.active_tab_mut();
                if tab.think_label.is_empty() && !label.is_empty() {
                    tab.think_start = Some(Instant::now());
                }
                tab.think_label = label;
            }
            TuiEvent::Tokens { input, output } => {
                let tab = self.active_tab_mut();
                tab.input_tokens = tab.input_tokens.saturating_add(input);
                tab.output_tokens = tab.output_tokens.saturating_add(output);
            }
            TuiEvent::System(msg) => {
                self.active_tab_mut().log.push(LogEntry::System(msg));
            }
            TuiEvent::TurnDone => {
                self.flush_pending_text();
                let tab = self.active_tab_mut();
                tab.think_label.clear();
                tab.think_start = None;
            }
        }
    }

    /// Auto-scroll to the bottom of the content.
    fn scroll_to_bottom(&mut self) {
        // We don't know total_lines here without rendering; set a high value and
        // `draw()` will clamp it.  This works because draw() does min(scroll, max_scroll).
        self.active_tab_mut().scroll = usize::MAX;
    }

    /// Scroll up by `n` lines.
    pub fn scroll_up(&mut self, n: usize) {
        let s = self.active_tab_mut().scroll;
        self.active_tab_mut().scroll = s.saturating_sub(n);
    }

    /// Scroll down by `n` lines (draw() clamps to max).
    pub fn scroll_down(&mut self, n: usize) {
        let s = self.active_tab_mut().scroll;
        self.active_tab_mut().scroll = s.saturating_add(n);
    }

    // ─── Input editing ───────────────────────────────────────────────────────

    fn insert_char(&mut self, ch: char) {
        let tab = self.active_tab_mut();
        tab.input.insert(tab.cursor, ch);
        tab.cursor += ch.len_utf8();
        tab.history_idx = None;
        tab.history_backup = None;
    }

    fn backspace(&mut self) {
        if self.active_tab().cursor == 0 {
            return;
        }
        let (cursor, input) = {
            let tab = self.active_tab();
            (tab.cursor, tab.input.clone())
        };
        let prev = prev_char_boundary(&input, cursor);
        let tab = self.active_tab_mut();
        tab.input.drain(prev..cursor);
        tab.cursor = prev;
        tab.history_idx = None;
        tab.history_backup = None;
    }

    fn delete_char(&mut self) {
        let (cursor, len) = {
            let tab = self.active_tab();
            (tab.cursor, tab.input.len())
        };
        if cursor >= len {
            return;
        }
        let next = {
            let input = self.active_tab().input.clone();
            next_char_boundary(&input, cursor)
        };
        self.active_tab_mut().input.drain(cursor..next);
    }

    fn cursor_left(&mut self) {
        let (cursor, input) = {
            let tab = self.active_tab();
            (tab.cursor, tab.input.clone())
        };
        if cursor > 0 {
            self.active_tab_mut().cursor = prev_char_boundary(&input, cursor);
        }
    }

    fn cursor_right(&mut self) {
        let (cursor, input) = {
            let tab = self.active_tab();
            (tab.cursor, tab.input.clone())
        };
        if cursor < input.len() {
            self.active_tab_mut().cursor = next_char_boundary(&input, cursor);
        }
    }

    fn cursor_home(&mut self) {
        self.active_tab_mut().cursor = 0;
    }

    fn cursor_end(&mut self) {
        let len = self.active_tab().input.len();
        self.active_tab_mut().cursor = len;
    }

    fn history_up(&mut self) {
        if self.active_tab().history.is_empty() {
            return;
        }
        let (idx, len) = {
            let tab = self.active_tab();
            (tab.history_idx, tab.history.len())
        };
        match idx {
            None => {
                let new_idx = len - 1;
                let entry = self.active_tab().history[new_idx].clone();
                let tab = self.active_tab_mut();
                tab.history_backup = Some(tab.input.clone());
                tab.history_idx = Some(new_idx);
                tab.input = entry;
            }
            Some(0) => {}
            Some(i) => {
                let new_idx = i - 1;
                let entry = self.active_tab().history[new_idx].clone();
                let tab = self.active_tab_mut();
                tab.history_idx = Some(new_idx);
                tab.input = entry;
            }
        }
        let len = self.active_tab().input.len();
        self.active_tab_mut().cursor = len;
    }

    fn history_down(&mut self) {
        let (idx, history_len) = {
            let tab = self.active_tab();
            (tab.history_idx, tab.history.len())
        };
        match idx {
            None => {}
            Some(i) => {
                if i + 1 >= history_len {
                    let backup = self.active_tab_mut().history_backup.take().unwrap_or_default();
                    let tab = self.active_tab_mut();
                    tab.history_idx = None;
                    tab.input = backup;
                } else {
                    let next_idx = i + 1;
                    let entry = self.active_tab().history[next_idx].clone();
                    let tab = self.active_tab_mut();
                    tab.history_idx = Some(next_idx);
                    tab.input = entry;
                }
                let len = self.active_tab().input.len();
                self.active_tab_mut().cursor = len;
            }
        }
    }

    fn submit_input(&mut self) -> Option<String> {
        let text = std::mem::take(&mut self.active_tab_mut().input);
        {
            let tab = self.active_tab_mut();
            tab.cursor = 0;
            tab.history_idx = None;
            tab.history_backup = None;
        }
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return None;
        }
        {
            let tab = self.active_tab_mut();
            tab.history.push(trimmed.clone());
            tab.log.push(LogEntry::User(trimmed.clone()));
        }
        self.scroll_to_bottom();
        Some(trimmed)
    }

    // ─── Main loop ───────────────────────────────────────────────────────────

    /// Run the interactive REPL loop.
    ///
    /// Returns `Ok(Some(input))` when the user submits a line.
    /// Returns `Ok(None)` when the user exits (`/exit`, Ctrl+C on empty, Ctrl+D).
    ///
    /// The caller is responsible for:
    ///   1. Calling `set_thinking(label)` before starting a turn.
    ///   2. Running the turn (which sends `TuiEvent`s over the channel).
    ///   3. Calling `wait_for_turn_end()` to process events until `TurnDone`.
    ///   4. Looping back to `read_input()` for the next prompt.
    pub fn read_input(&mut self) -> io::Result<ReadResult> {
        // Tick the think frame index so the spinner animates even during input.
        self.active_tab_mut().think_frame = self.active_tab().think_frame.wrapping_add(1);
        self.draw()?;

        // Use a short poll timeout so we can process model events and animate the spinner.
        if event::poll(Duration::from_millis(80))? {
            if let CtEvent::Key(key) = event::read()? {
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    return self.handle_key(key);
                }
            } else if let CtEvent::Resize(_, _) = event::read().unwrap_or(CtEvent::FocusLost) {
                // Terminal resized; redraw on next tick.
            }
        }

        Ok(ReadResult::Continue)
    }

    fn handle_key(&mut self, key: KeyEvent) -> io::Result<ReadResult> {
        // Ctrl combos
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.handle_ctrl_key(key);
        }
        // Alt combos — Alt+1..9 switch tabs
        if key.modifiers.contains(KeyModifiers::ALT) {
            if let KeyCode::Char(ch) = key.code {
                if let Some(n) = ch.to_digit(10) {
                    if n >= 1 {
                        self.switch_tab((n as usize).saturating_sub(1));
                        return Ok(ReadResult::Continue);
                    }
                }
            }
        }

        match key.code {
            KeyCode::Enter => {
                if self.active_tab().completion.visible {
                    // Accept selected completion on Enter
                    self.tab_complete();
                    self.active_tab_mut().completion = CompletionPopup::default();
                } else if let Some(line) = self.submit_input() {
                    self.active_tab_mut().completion = CompletionPopup::default();
                    return Ok(ReadResult::Submit(line));
                }
            }
            KeyCode::Backspace => {
                self.backspace();
                self.refresh_completion();
            }
            KeyCode::Delete => self.delete_char(),
            KeyCode::Left => self.cursor_left(),
            KeyCode::Right => self.cursor_right(),
            KeyCode::Home => self.cursor_home(),
            KeyCode::End => self.cursor_end(),
            KeyCode::Up => {
                if self.active_tab().completion.visible {
                    self.completion_up();
                } else if self.active_tab().think_label.is_empty() {
                    self.history_up();
                } else {
                    self.scroll_up(3);
                }
            }
            KeyCode::Down => {
                if self.active_tab().completion.visible {
                    self.completion_down();
                } else if self.active_tab().think_label.is_empty() {
                    self.history_down();
                } else {
                    self.scroll_down(3);
                }
            }
            KeyCode::PageUp => self.scroll_up(10),
            KeyCode::PageDown => self.scroll_down(10),
            KeyCode::Char(ch) => {
                self.insert_char(ch);
                self.refresh_completion();
            }
            KeyCode::Tab => {
                self.tab_complete();
            }
            KeyCode::Esc => {
                if self.active_tab().completion.visible {
                    self.active_tab_mut().completion = CompletionPopup::default();
                }
            }
            _ => {}
        }
        Ok(ReadResult::Continue)
    }

    fn handle_ctrl_key(&mut self, key: KeyEvent) -> io::Result<ReadResult> {
        match key.code {
            // ── Tab management ──────────────────────────────────────────────
            KeyCode::Char('t') | KeyCode::Char('T') => {
                // Ctrl+T: new tab (caller must create a session and call new_tab)
                return Ok(ReadResult::NewTab);
            }
            KeyCode::Char('w') | KeyCode::Char('W') => {
                // Ctrl+W: close current tab (last tab cannot be closed)
                if self.tabs.len() > 1 {
                    if let Some(name) = self.close_active_tab() {
                        self.push_system(format!("Closed tab: {name}"));
                    }
                } else {
                    self.push_system("Cannot close the last tab.".to_string());
                }
            }
            // Tab switching: Ctrl+Right/Left (Linux), Ctrl+]/[ (macOS-friendly)
            KeyCode::Right => {
                self.next_tab();
            }
            KeyCode::Left => {
                self.prev_tab();
            }
            KeyCode::Char(']') => {
                self.next_tab();
            }
            KeyCode::Char('[') => {
                self.prev_tab();
            }
            // Ctrl+N/P for next/prev tab when input is empty
            KeyCode::Char('n') | KeyCode::Char('N') if self.active_tab().input.is_empty() => {
                self.next_tab();
            }
            // Ctrl+1..9 — switch to tab N (1-based)
            KeyCode::Char(ch) if ch.is_ascii_digit() && ch != '0' => {
                let n = ch as usize - '0' as usize;
                self.switch_tab(n.saturating_sub(1));
            }
            // ── Existing ctrl bindings ───────────────────────────────────────
            KeyCode::Char('c') | KeyCode::Char('C') => {
                if self.active_tab().input.is_empty() {
                    return Ok(ReadResult::Exit);
                }
                let tab = self.active_tab_mut();
                tab.input.clear();
                tab.cursor = 0;
                tab.history_idx = None;
                tab.history_backup = None;
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                if self.active_tab().input.is_empty() {
                    return Ok(ReadResult::Exit);
                }
                self.delete_char();
            }
            KeyCode::Char('u') | KeyCode::Char('U') => {
                // Kill to beginning of line
                let cursor = self.active_tab().cursor;
                self.active_tab_mut().input.drain(..cursor);
                self.active_tab_mut().cursor = 0;
            }
            KeyCode::Char('k') | KeyCode::Char('K') => {
                // Kill to end of line
                let cursor = self.active_tab().cursor;
                self.active_tab_mut().input.truncate(cursor);
            }
            KeyCode::Char('a') | KeyCode::Char('A') => self.cursor_home(),
            KeyCode::Char('e') | KeyCode::Char('E') => self.cursor_end(),
            KeyCode::Char('j') | KeyCode::Char('J') => {
                // Ctrl+J = newline in input
                self.insert_char('\n');
            }
            KeyCode::Char('p') | KeyCode::Char('P') => self.history_up(),
            KeyCode::Char('n') | KeyCode::Char('N') => self.history_down(),
            _ => {}
        }
        Ok(ReadResult::Continue)
    }

    fn tab_complete(&mut self) {
        let (visible, matches_empty) = {
            let c = &self.active_tab().completion;
            (c.visible, c.matches.is_empty())
        };
        if !visible || matches_empty {
            // If popup not visible, try to open it
            let input = self.active_tab().input.clone();
            self.active_tab_mut().completion = update_completions(&input);
            return;
        }

        // Accept the selected completion
        let (insert, ends_with_space, first_part) = {
            let tab = &self.active_tab().completion;
            let selected = tab.matches[tab.selected].insert.clone();
            let input = self.active_tab().input.clone();
            let ends_space = input.ends_with(' ');
            let first = input.splitn(2, ' ').next().unwrap_or("").to_string();
            (selected, ends_space, first)
        };
        let parts_len = self.active_tab().input.splitn(2, ' ').count();
        let new_input = if parts_len == 1 && !ends_with_space {
            format!("{insert} ")
        } else {
            format!("{first_part} {insert}")
        };
        let new_len = new_input.len();
        let tab = self.active_tab_mut();
        tab.input = new_input.clone();
        tab.cursor = new_len;
        tab.completion = update_completions(&new_input);
    }

    fn completion_up(&mut self) {
        let tab = self.active_tab_mut();
        if tab.completion.visible && !tab.completion.matches.is_empty() {
            if tab.completion.selected > 0 {
                tab.completion.selected -= 1;
            } else {
                tab.completion.selected = tab.completion.matches.len() - 1;
            }
        }
    }

    fn completion_down(&mut self) {
        let tab = self.active_tab_mut();
        if tab.completion.visible && !tab.completion.matches.is_empty() {
            if tab.completion.selected + 1 < tab.completion.matches.len() {
                tab.completion.selected += 1;
            } else {
                tab.completion.selected = 0;
            }
        }
    }

    fn refresh_completion(&mut self) {
        let input = self.active_tab().input.clone();
        self.active_tab_mut().completion = update_completions(&input);
    }

    /// Process all queued model events (non-blocking).  Call this periodically
    /// during a running turn so the display stays live.
    pub fn poll_events(&mut self) {
        self.drain_events();
        let frame = self.active_tab().think_frame.wrapping_add(1);
        self.active_tab_mut().think_frame = frame;
    }

    /// Block until `TurnDone` arrives on the channel, processing events as they
    /// come and redrawing the TUI.  Returns when the turn finishes.
    pub fn wait_for_turn_end(&mut self) -> io::Result<()> {
        loop {
            // Drain non-blocking first
            loop {
                match self.rx.try_recv() {
                    Ok(TuiEvent::TurnDone) => {
                        self.apply_tui_event(TuiEvent::TurnDone);
                        self.scroll_to_bottom();
                        self.draw()?;
                        return Ok(());
                    }
                    Ok(event) => self.apply_tui_event(event),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        // Pipeline hung up; treat as done
                        self.flush_pending_text();
                        {
                            let tab = self.active_tab_mut();
                            tab.think_label.clear();
                        }
                        self.scroll_to_bottom();
                        self.draw()?;
                        return Ok(());
                    }
                }
            }

            {
                let frame = self.active_tab().think_frame.wrapping_add(1);
                self.active_tab_mut().think_frame = frame;
            }
            self.draw()?;

            // Block with timeout so we can animate the spinner
            match self.rx.recv_timeout(Duration::from_millis(80)) {
                Ok(TuiEvent::TurnDone) => {
                    self.apply_tui_event(TuiEvent::TurnDone);
                    self.scroll_to_bottom();
                    self.draw()?;
                    return Ok(());
                }
                Ok(event) => self.apply_tui_event(event),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.flush_pending_text();
                    {
                        let tab = self.active_tab_mut();
                        tab.think_label.clear();
                    }
                    self.scroll_to_bottom();
                    self.draw()?;
                    return Ok(());
                }
            }
        }
    }

    /// Show a system message (e.g. slash command output).
    pub fn push_system(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !text.is_empty() {
            self.active_tab_mut().log.push(LogEntry::System(text));
        }
        self.scroll_to_bottom();
    }

    /// Update the displayed permission mode (e.g. after /permissions switch).
    #[allow(dead_code)]
    pub fn set_permission_mode(&mut self, mode: impl Into<String>) {
        self.permission_mode = mode.into();
    }

    /// Re-run git queries and update cached branch/diff info.
    #[allow(dead_code)]
    pub fn refresh_git_info(&mut self) {
        let (branch, diff) = fetch_git_info();
        self.git_branch = branch;
        self.git_diff_stats = diff;
    }

    /// Update the QMD status line in the footer.
    pub fn set_qmd_status(&mut self, status: impl Into<String>) {
        self.qmd_status = status.into();
    }

    /// Update the last archive status shown in the footer.
    pub fn set_archive_status(&mut self, status: impl Into<String>) {
        self.last_archive_status = status.into();
    }

    /// Signal that the TUI should close on the next `read_input` loop tick.
    #[allow(dead_code)]
    pub fn request_exit(&mut self) {
        self.exiting = true;
    }

    /// True if `request_exit()` was called.
    #[allow(dead_code)]
    pub fn is_exiting(&self) -> bool {
        self.exiting
    }
}

impl Drop for AnvilTui {
    fn drop(&mut self) {
        // Best-effort cleanup; ignore errors during drop.
        let _ = terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            io::stdout(),
            terminal::LeaveAlternateScreen
        );
    }
}

// ─── ReadResult ───────────────────────────────────────────────────────────────

/// The result of one call to `AnvilTui::read_input()`.
pub enum ReadResult {
    /// Nothing to report; caller should call `read_input()` again.
    Continue,
    /// User submitted this line.
    Submit(String),
    /// User requested exit (Ctrl+C on empty input, Ctrl+D on empty input).
    Exit,
    /// User requested a new tab (Ctrl+T).  Caller should create a new session
    /// and call `tui.new_tab(name, model, session_id)` then `tui.switch_tab(idx)`.
    NewTab,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Remove ANSI escape codes from a string for plain text rendering.
fn strip_ansi(s: &str) -> String {
    // Simple state-machine based stripper.
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Consume the escape sequence
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                // consume up to and including the final byte (letter)
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}…")
}

fn prev_char_boundary(s: &str, mut pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    pos -= 1;
    while pos > 0 && !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

fn next_char_boundary(s: &str, mut pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    pos += 1;
    while pos < s.len() && !s.is_char_boundary(pos) {
        pos += 1;
    }
    pos
}

/// Fetch git branch and diff stats for the current working directory.
/// Returns empty strings on any failure (not in a git repo, git not installed, etc.).
fn fetch_git_info() -> (String, String) {
    use std::process::Command;

    let branch = Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();

    // git diff --shortstat gives e.g. " 3 files changed, 12 insertions(+), 5 deletions(-)"
    // We parse that into "+12,-5".
    let diff_stats = Command::new("git")
        .args(["diff", "--shortstat"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| parse_shortstat(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_default();

    (branch, diff_stats)
}

/// Parse the output of `git diff --shortstat` into a compact "+N,-M" string.
fn parse_shortstat(s: &str) -> String {
    let mut ins: u32 = 0;
    let mut del: u32 = 0;
    for part in s.split(',') {
        let part = part.trim();
        if part.contains("insertion") {
            if let Some(n) = part.split_whitespace().next() {
                ins = n.parse().unwrap_or(0);
            }
        } else if part.contains("deletion") {
            if let Some(n) = part.split_whitespace().next() {
                del = n.parse().unwrap_or(0);
            }
        }
    }
    if ins == 0 && del == 0 {
        String::new()
    } else {
        format!("+{ins},-{del}")
    }
}

/// Return the approximate context window size (in tokens) for a known model.
fn context_max_for_model(model: &str) -> u32 {
    // Check ANVIL_CONTEXT_SIZE env var first (user override)
    if let Ok(val) = std::env::var("ANVIL_CONTEXT_SIZE") {
        if let Ok(n) = val.replace("k", "000").replace("K", "000")
            .replace("m", "000000").replace("M", "000000")
            .parse::<u32>() {
            return n;
        }
    }

    let m = model.to_lowercase();
    if m.contains("opus") {
        1_000_000 // Opus 4.6 supports 1M context
    } else if m.contains("sonnet") {
        1_000_000 // Sonnet 4.6 supports 1M context
    } else if m.contains("haiku") {
        200_000
    } else if m.starts_with("gpt-4o") {
        128_000
    } else if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
        200_000
    } else {
        1_000_000 // default to 1M
    }
}

/// Convert the internal permission mode string to a human-readable display string.
fn permission_mode_display(mode: &str) -> String {
    match mode {
        "read-only" => "read-only mode on".to_string(),
        "workspace-write" => "workspace-write mode on".to_string(),
        "danger-full-access" => "bypass permissions on".to_string(),
        "prompt" => "prompt mode on".to_string(),
        "allow" => "allow mode on".to_string(),
        other if other.is_empty() => "bypass permissions on".to_string(),
        other => format!("{other} mode on"),
    }
}

/// Build a ratatui `Line` with left-aligned spans and right-aligned spans,
/// padding the middle with spaces to fill the terminal width.
fn build_left_right_line(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
) -> Line<'static> {
    let left_len: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_len: usize = right.iter().map(|s| s.content.chars().count()).sum();
    let pad = width.saturating_sub(left_len + right_len);
    let padding = Span::raw(" ".repeat(pad));
    let mut spans = left;
    spans.push(padding);
    spans.extend(right);
    Line::from(spans)
}

/// Build the left spans for status line 1 with model name in yellow,
/// git branch in green, and diff stats in dim white.
fn build_status1_spans(
    model: &str,
    total_m: f64,
    git_branch: &str,
    git_diff: &str,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled("Model: ", Style::default().fg(Color::Rgb(0x88, 0x88, 0x88))),
        Span::styled(model.to_string(), Style::default().fg(Color::Yellow)),
        Span::styled(
            format!(" | Total: {total_m:.1}M"),
            Style::default().fg(Color::Rgb(0x88, 0x88, 0x88)),
        ),
    ];
    if !git_branch.is_empty() {
        spans.push(Span::styled(
            " | ⌐".to_string(),
            Style::default().fg(Color::Rgb(0x88, 0x88, 0x88)),
        ));
        spans.push(Span::styled(
            git_branch.to_string(),
            Style::default().fg(Color::Green),
        ));
    }
    if !git_diff.is_empty() {
        spans.push(Span::styled(
            format!(" | ({git_diff})"),
            Style::default().fg(Color::Rgb(0x88, 0x88, 0x88)),
        ));
    }
    spans
}

/// All top-level slash commands with short descriptions.
fn all_slash_commands() -> Vec<CompletionItem> {
    vec![
        CompletionItem { insert: "/help".into(), hint: "Show available commands".into() },
        CompletionItem { insert: "/status".into(), hint: "Session + workspace status".into() },
        CompletionItem { insert: "/model".into(), hint: "Show or switch model".into() },
        CompletionItem { insert: "/provider".into(), hint: "Switch provider (anthropic/openai/ollama)".into() },
        CompletionItem { insert: "/login".into(), hint: "Login/refresh provider credentials".into() },
        CompletionItem { insert: "/compact".into(), hint: "Compact session history".into() },
        CompletionItem { insert: "/clear".into(), hint: "Start a fresh session".into() },
        CompletionItem { insert: "/cost".into(), hint: "Token usage for this session".into() },
        CompletionItem { insert: "/tokens".into(), hint: "Detailed token breakdown".into() },
        CompletionItem { insert: "/diff".into(), hint: "Show git diff".into() },
        CompletionItem { insert: "/version".into(), hint: "CLI version info".into() },
        CompletionItem { insert: "/memory".into(), hint: "Loaded memory files".into() },
        CompletionItem { insert: "/config".into(), hint: "Inspect configuration".into() },
        CompletionItem { insert: "/export".into(), hint: "Export conversation".into() },
        CompletionItem { insert: "/session".into(), hint: "List or switch sessions".into() },
        CompletionItem { insert: "/permissions".into(), hint: "Show or set permission mode".into() },
        CompletionItem { insert: "/init".into(), hint: "Scaffold ANVIL.md + config".into() },
        CompletionItem { insert: "/commit".into(), hint: "Generate commit message + commit".into() },
        CompletionItem { insert: "/commit-push-pr".into(), hint: "Commit, push, and open PR".into() },
        CompletionItem { insert: "/pr".into(), hint: "Draft or create a pull request".into() },
        CompletionItem { insert: "/issue".into(), hint: "Draft or create a GitHub issue".into() },
        CompletionItem { insert: "/branch".into(), hint: "List, create, or switch branches".into() },
        CompletionItem { insert: "/worktree".into(), hint: "Manage git worktrees".into() },
        CompletionItem { insert: "/bughunter".into(), hint: "Scan codebase for bugs".into() },
        CompletionItem { insert: "/ultraplan".into(), hint: "Deep planning with reasoning".into() },
        CompletionItem { insert: "/teleport".into(), hint: "Jump to a file or symbol".into() },
        CompletionItem { insert: "/qmd".into(), hint: "Search knowledge base".into() },
        CompletionItem { insert: "/doctor".into(), hint: "Diagnose configuration".into() },
        CompletionItem { insert: "/context".into(), hint: "Add file to context".into() },
        CompletionItem { insert: "/pin".into(), hint: "Pin file to always-in-context".into() },
        CompletionItem { insert: "/unpin".into(), hint: "Remove pinned file".into() },
        CompletionItem { insert: "/undo".into(), hint: "Undo last file changes".into() },
        CompletionItem { insert: "/history".into(), hint: "Show conversation history".into() },
        CompletionItem { insert: "/chat".into(), hint: "Toggle chat-only mode (no tools)".into() },
        CompletionItem { insert: "/vim".into(), hint: "Toggle vim keybindings".into() },
        CompletionItem { insert: "/web".into(), hint: "Quick web search".into() },
        CompletionItem { insert: "/agents".into(), hint: "List configured agents".into() },
        CompletionItem { insert: "/skills".into(), hint: "List available skills".into() },
        CompletionItem { insert: "/plugins".into(), hint: "Manage plugins".into() },
        CompletionItem { insert: "/debug-tool-call".into(), hint: "Replay last tool call".into() },
        CompletionItem { insert: "/exit".into(), hint: "Exit Anvil".into() },
        CompletionItem { insert: "/tab".into(), hint: "Manage tabs (new/close/list/rename)".into() },
    ]
}

/// Sub-command completions for commands that have them.
fn subcommands_for(command: &str) -> Vec<CompletionItem> {
    match command {
        "/provider" | "/providers" => vec![
            CompletionItem { insert: "list".into(), hint: "List models for current provider".into() },
            CompletionItem { insert: "anthropic".into(), hint: "Switch to Anthropic (Claude)".into() },
            CompletionItem { insert: "openai".into(), hint: "Switch to OpenAI (GPT)".into() },
            CompletionItem { insert: "ollama".into(), hint: "Switch to Ollama (local)".into() },
            CompletionItem { insert: "xai".into(), hint: "Switch to xAI (Grok)".into() },
            CompletionItem { insert: "login".into(), hint: "Login/refresh current provider".into() },
        ],
        "/login" => vec![
            CompletionItem { insert: "anthropic".into(), hint: "Login to Anthropic (OAuth)".into() },
            CompletionItem { insert: "openai".into(), hint: "Setup OpenAI API key".into() },
            CompletionItem { insert: "ollama".into(), hint: "Configure Ollama endpoint".into() },
        ],
        "/config" => vec![
            CompletionItem { insert: "env".into(), hint: "Show environment config".into() },
            CompletionItem { insert: "hooks".into(), hint: "Show hook config".into() },
            CompletionItem { insert: "model".into(), hint: "Show model config".into() },
            CompletionItem { insert: "plugins".into(), hint: "Show plugin config".into() },
        ],
        "/permissions" => vec![
            CompletionItem { insert: "read-only".into(), hint: "Read-only access".into() },
            CompletionItem { insert: "workspace-write".into(), hint: "Write within workspace".into() },
            CompletionItem { insert: "danger-full-access".into(), hint: "Full access (no restrictions)".into() },
        ],
        "/session" => vec![
            CompletionItem { insert: "list".into(), hint: "List saved sessions".into() },
            CompletionItem { insert: "switch".into(), hint: "Switch to a session".into() },
        ],
        "/branch" => vec![
            CompletionItem { insert: "list".into(), hint: "List branches".into() },
            CompletionItem { insert: "create".into(), hint: "Create a new branch".into() },
            CompletionItem { insert: "switch".into(), hint: "Switch to a branch".into() },
        ],
        "/worktree" => vec![
            CompletionItem { insert: "list".into(), hint: "List worktrees".into() },
            CompletionItem { insert: "add".into(), hint: "Add a worktree".into() },
            CompletionItem { insert: "remove".into(), hint: "Remove a worktree".into() },
            CompletionItem { insert: "prune".into(), hint: "Prune stale worktrees".into() },
        ],
        "/plugins" | "/plugin" => vec![
            CompletionItem { insert: "list".into(), hint: "List installed plugins".into() },
            CompletionItem { insert: "install".into(), hint: "Install a plugin".into() },
            CompletionItem { insert: "enable".into(), hint: "Enable a plugin".into() },
            CompletionItem { insert: "disable".into(), hint: "Disable a plugin".into() },
            CompletionItem { insert: "uninstall".into(), hint: "Uninstall a plugin".into() },
        ],
        "/tab" => vec![
            CompletionItem { insert: "new".into(), hint: "Open a new tab".into() },
            CompletionItem { insert: "close".into(), hint: "Close the current tab".into() },
            CompletionItem { insert: "list".into(), hint: "List all open tabs".into() },
            CompletionItem { insert: "rename".into(), hint: "Rename the current tab".into() },
        ],
        "/clear" => vec![
            CompletionItem { insert: "--confirm".into(), hint: "Confirm session clear".into() },
        ],
        "/history" => vec![
            CompletionItem { insert: "all".into(), hint: "Show full history".into() },
        ],
        _ => vec![],
    }
}

/// Update the completion popup based on current input.
fn update_completions(input: &str) -> CompletionPopup {
    if input.is_empty() || !input.starts_with('/') {
        return CompletionPopup::default();
    }

    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let command = parts[0];

    if parts.len() == 1 && !input.ends_with(' ') {
        // Typing a command name — filter top-level commands
        let matches: Vec<CompletionItem> = all_slash_commands()
            .into_iter()
            .filter(|c| c.insert.starts_with(input))
            .collect();
        if matches.is_empty() || (matches.len() == 1 && matches[0].insert == input) {
            return CompletionPopup::default();
        }
        CompletionPopup {
            visible: true,
            matches,
            selected: 0,
        }
    } else {
        // After a space — show sub-commands
        let sub_prefix = parts.get(1).unwrap_or(&"").trim();
        let subs = subcommands_for(command);
        if subs.is_empty() {
            return CompletionPopup::default();
        }
        let matches: Vec<CompletionItem> = if sub_prefix.is_empty() {
            subs
        } else {
            subs.into_iter()
                .filter(|c| c.insert.starts_with(sub_prefix))
                .collect()
        };
        if matches.is_empty() {
            return CompletionPopup::default();
        }
        CompletionPopup {
            visible: true,
            matches,
            selected: 0,
        }
    }
}
