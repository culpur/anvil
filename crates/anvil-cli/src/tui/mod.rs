//! Full-screen TUI for Anvil — ratatui-based alternate-screen layout.
//!
//! This is the module root.  Submodules:
//!   state            — `TuiEvent`, `TuiSender`, `LogEntry`, `Tab`, `CompletionPopup`, `THINK_FRAMES`
//!   helpers          — `strip_ansi`, `truncate_str`, char-boundary helpers, `permission_mode_display`
//!   layout           — `compute_input_lines`, `cursor_visual_position`, status-line span builders
//!   widgets          — slash-command completion data, Ollama model cache, clipboard helpers
//!   `configure_types`  — `ConfigureState`, `ConfigureAction`, `ConfigureData`, configure menu helpers
//!   `input_handler`    — `AnvilTui` input loop (`read_input`, `handle_key`, editing, history, completion)
pub mod configure_types;
pub(super) mod helpers;
pub(super) mod layout;
pub(super) mod state;
pub(super) mod widgets;
pub(super) mod input_handler;

// ─── Public re-exports ────────────────────────────────────────────────────────

pub use state::{TuiEvent, TuiSender};
pub use configure_types::{ConfigureAction, ConfigureData};
pub use widgets::init_ollama_model_cache;


// Internal imports used by AnvilTui methods in this file.
use configure_types::{
    ConfigureState,
    configure_breadcrumb, configure_selected, configure_set_selected,
    configure_item_count, section_state_from_name,
    configure_action_for, configure_data_notify_value, mask_sensitive,
};
use helpers::{strip_ansi, truncate_str, permission_mode_display, prev_char_boundary, next_char_boundary};
use layout::{compute_input_lines, cursor_visual_position, build_left_right_line, build_status1_spans};
use state::{Tab, LogEntry, THINK_FRAMES};


use std::io::{self, Stdout};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use runtime::{format_usd, pricing_for_model, Rgb, Theme};

use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;

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
    /// User triggered a configure action from the interactive configure menu.
    ConfigureAction(ConfigureAction),
}

// ─── AnvilTui ─────────────────────────────────────────────────────────────────

/// Convert a runtime `Rgb` triple into a ratatui `Color`.
#[inline]
fn rgb(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// The full-screen TUI driver.
///
/// Create with `AnvilTui::new()`, then call `run()` to enter the main loop.
/// The caller passes prompts back via the returned `String` from `run()`.
pub struct AnvilTui {
    pub(super) terminal: Terminal<CrosstermBackend<Stdout>>,
    /// All open tabs.
    pub(super) tabs: Vec<Tab>,
    /// Index into `tabs` of the currently visible tab.
    pub(super) active_tab: usize,
    /// Channel receiver from the model/tool pipeline.
    pub(super) rx: Receiver<TuiEvent>,
    /// True once /exit or Ctrl+D has been issued.
    pub(super) exiting: bool,
    /// Current git branch name (empty if not in a git repo).
    pub(super) git_branch: String,
    /// Compact diff stats string e.g. "+12,-5" (empty if no diff or not in git repo).
    pub(super) git_diff_stats: String,
    /// Current permission mode display label.
    pub(super) permission_mode: String,
    /// Maximum context window tokens for the current model.
    pub(super) context_max_tokens: u32,
    /// Running counter for assigning tab IDs.
    pub(super) next_tab_id: usize,
    /// QMD status line: docs indexed, vectors, last update
    pub(super) qmd_status: String,
    /// Last archive info shown to user
    pub(super) last_archive_status: String,
    /// Current configure menu state (Inactive = not in configure mode).
    pub(super) configure_state: ConfigureState,
    /// Snapshot of live config values shown in the configure menu.
    pub(super) configure_data: ConfigureData,
    /// Active colour theme — loaded from ~/.anvil/theme.json at startup.
    pub theme: Theme,
    /// Update notification message (empty = no update available).
    pub(super) update_available: String,
    /// Scroll offset for content area (0 = at bottom, >0 = scrolled up)
    #[allow(dead_code)]
    pub(super) content_scroll_offset: usize,
    /// Whether auto-scroll is active (disabled when user scrolls up)
    #[allow(dead_code)]
    pub(super) content_auto_scroll: bool,
    /// Whether the agent panel is visible (toggled with Ctrl+A).
    pub agent_panel_visible: bool,
    /// Agent panel rows snapshot — refreshed by the caller each frame.
    /// Each entry: (id, `type_label`, task, `elapsed_str`, icon).
    pub agent_rows: Vec<(usize, String, String, String, &'static str)>,
    /// Timestamp of the last Ctrl+C that fired while the input was already
    /// empty.  A second Ctrl+C within 1 second exits; otherwise it resets.
    pub(super) ctrl_c_empty_at: Option<Instant>,
}

impl AnvilTui {
    /// Enter alternate screen and return the TUI + the sender for model events.
    pub fn new(
        model: impl Into<String>,
        session_id: impl Into<String>,
        permission_mode: impl Into<String>,
    ) -> io::Result<(Self, TuiSender)> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        crossterm::execute!(
            stdout,
            terminal::EnterAlternateScreen,
            crossterm::event::EnableMouseCapture,
            crossterm::event::EnableBracketedPaste
        )?;
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
                configure_state: ConfigureState::Inactive,
                configure_data: ConfigureData::default(),
                theme: Theme::load(),
                update_available: String::new(),
                content_scroll_offset: 0,
                content_auto_scroll: true,
                agent_panel_visible: true,
                agent_rows: Vec::new(),
                ctrl_c_empty_at: None,
            },
            TuiSender(tx),
        ))
    }

    // ─── Tab accessors ───────────────────────────────────────────────────────

    pub(super) fn active_tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    pub(super) fn active_tab_mut(&mut self) -> &mut Tab {
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
    pub(super) fn next_tab(&mut self) {
        let next = (self.active_tab + 1) % self.tabs.len();
        self.switch_tab(next);
    }

    /// Switch to the previous tab (wraps around).
    pub(super) fn prev_tab(&mut self) {
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

    /// Return a list of (index, id, name, `has_unread`) tuples.
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

    /// Apply a new theme to the TUI immediately (live hot-swap).
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    // ─── Draw ────────────────────────────────────────────────────────────────

    /// Draw the current state.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    pub(super) fn draw(&mut self) -> io::Result<()> {
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
        let update_available = self.update_available.clone();
        let configure_state = self.configure_state.clone();
        let configure_data = self.configure_data.clone();
        let theme = self.theme.clone();
        let agent_panel_visible = self.agent_panel_visible;
        let agent_rows = self.agent_rows.clone();

        self.terminal.draw(|frame| {
            let size = frame.area();
            let width = size.width as usize;

            // ── layout ──────────────────────────────────────────────────────
            // header=2 (tab bar + model/session line), content=fill,
            // [agent panel = 2+N lines when visible and agents exist],
            // footer = 5 + input_line_count  (grows 6–10 as input expands)

            // How many visual rows does the current input occupy? (1–5)
            let input_line_count = compute_input_lines(&input_text, width);
            // Total footer height: separator + input rows + blank + 3 status lines.
            let footer_height: u16 = (5 + input_line_count) as u16;

            // Agent panel height: 2 lines for border + 1 per agent row (max 6).
            let agent_panel_height: u16 = if agent_panel_visible && !agent_rows.is_empty() {
                (agent_rows.len().min(6) as u16) + 2
            } else {
                0
            };

            let chunks = if agent_panel_height > 0 {
                Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(2),
                        Constraint::Min(4),
                        Constraint::Length(agent_panel_height),
                        Constraint::Length(footer_height),
                    ])
                    .split(size)
            } else {
                // No agent panel — use the 3-zone layout.
                let base = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(2),
                        Constraint::Min(4),
                        Constraint::Length(footer_height),
                    ])
                    .split(size);
                // Return a 4-element slice by padding (agent area = zero-height at footer y).
                let mut v = base.to_vec();
                v.push(v[2]);  // duplicate footer slot as placeholder
                v.into()
            };

            let header_area = chunks[0];
            let content_area = chunks[1];
            let (agent_panel_area, footer_area) = if agent_panel_height > 0 {
                (Some(chunks[2]), chunks[3])
            } else {
                (None, chunks[2])
            };

            // Split header into two rows: tab bar (row 0) + model/session (row 1).
            let header_rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Length(1)])
                .split(header_area);
            let tab_bar_area = header_rows[0];
            let model_bar_area = header_rows[1];

            // ── tab bar (row 0) ──────────────────────────────────────────────
            let mut tab_spans: Vec<Span<'static>> = vec![Span::raw(" ")];
            for (tab_id, tab_name, is_active, has_unread) in &tab_infos {
                let label = if *has_unread {
                    format!("[{tab_id}: {tab_name}*]")
                } else {
                    format!("[{tab_id}: {tab_name}]")
                };
                let style = if *is_active {
                    Style::default()
                        .fg(rgb(theme.accent))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(rgb(theme.text_secondary))
                        .add_modifier(Modifier::DIM)
                };
                tab_spans.push(Span::styled(label, style));
                tab_spans.push(Span::raw(" "));
            }
            // Hint on the right side of the tab bar
            let hint = Span::styled(
                "Ctrl+T new  Ctrl+W close  Ctrl+←/→ switch",
                Style::default().fg(rgb(theme.border)),
            );
            let tab_bar_left_len: usize = tab_spans.iter().map(|s| s.content.chars().count()).sum();
            let hint_len = hint.content.chars().count();
            let pad = width.saturating_sub(tab_bar_left_len + hint_len);
            tab_spans.push(Span::raw(" ".repeat(pad)));
            tab_spans.push(hint);
            let tab_bar_widget = Paragraph::new(Line::from(tab_spans))
                .style(Style::default().bg(rgb(theme.bg_primary)));
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
                    .fg(rgb(theme.accent))
                    .bg(rgb(theme.header_bg))
                    .add_modifier(Modifier::BOLD),
            );
            frame.render_widget(model_bar, model_bar_area);

            // ── content ─────────────────────────────────────────────────────
            let content_width = content_area.width;

            let all_lines: Vec<Line<'static>> = if configure_state == ConfigureState::Inactive {
                let mut lines: Vec<Line<'static>> = Vec::new();

                for entry in &log_snapshot {
                    lines.extend(entry.to_lines(content_width, &theme));
                }

                // Streaming assistant text in progress
                if !pending.is_empty() {
                    let clean = strip_ansi(&pending);
                    lines.extend(
                        clean
                            .lines()
                            .map(|l| Line::from(Span::raw(l.to_string()))),
                    );
                }

                // Thinking spinner
                if !think.is_empty() {
                    let elapsed_think = format!("{:.1}s", {
                        think_frame.len() as f64 * 0.25
                    });
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("{think_frame} "),
                            Style::default().fg(rgb(theme.thinking)),
                        ),
                        Span::styled(
                            think.clone(),
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::ITALIC),
                        ),
                        Span::styled(
                            format!("  ({elapsed_think})"),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }

                lines
            } else {
                // Configure mode — render the menu instead of the conversation log.
                render_configure_menu(&configure_state, &configure_data, content_width as usize)
            };

            let total_lines = all_lines.len();
            let visible_height = content_area.height as usize;
            let effective_scroll = if configure_state == ConfigureState::Inactive {
                let max_scroll = total_lines.saturating_sub(visible_height);
                scroll.min(max_scroll)
            } else {
                0
            };

            let visible_lines: Vec<Line<'static>> = all_lines
                .into_iter()
                .skip(effective_scroll)
                .take(visible_height)
                .collect();

            // Clear the content area first.
            frame.render_widget(ratatui::widgets::Clear, content_area);

            // Truncate each line to content_area.width.
            let max_col = content_area.width as usize;
            let visible_lines: Vec<Line<'static>> = visible_lines
                .into_iter()
                .map(|line| {
                    let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
                    if total <= max_col {
                        line
                    } else {
                        let mut remaining = max_col.saturating_sub(1);
                        let mut new_spans: Vec<Span<'static>> = Vec::new();
                        for span in line.spans {
                            if remaining == 0 {
                                break;
                            }
                            let chars: usize = span.content.chars().count();
                            if chars <= remaining {
                                remaining -= chars;
                                new_spans.push(span);
                            } else {
                                let truncated: String = span.content.chars().take(remaining).collect();
                                new_spans.push(Span::styled(
                                    format!("{truncated}…"),
                                    span.style,
                                ));
                                remaining = 0;
                            }
                        }
                        Line::from(new_spans)
                    }
                })
                .collect();

            let content_widget =
                Paragraph::new(Text::from(visible_lines)).style(Style::default().fg(Color::White));
            frame.render_widget(content_widget, content_area);

            // ── agent panel ──────────────────────────────────────────────────
            if let Some(panel_area) = agent_panel_area {
                frame.render_widget(ratatui::widgets::Clear, panel_area);
                let panel_width = panel_area.width as usize;

                let running = agent_rows.iter().filter(|r| r.4 == "⟳").count();
                let done = agent_rows.iter().filter(|r| r.4 == "✓").count();
                let failed = agent_rows.iter().filter(|r| r.4 == "✗").count();
                let mut status_parts = Vec::new();
                if running > 0 { status_parts.push(format!("{running} running")); }
                if done > 0 { status_parts.push(format!("{done} completed")); }
                if failed > 0 { status_parts.push(format!("{failed} failed")); }
                let status_str = status_parts.join(", ");
                let header_label = format!(" Agents ({status_str}) ");
                let dashes_after = "─".repeat(
                    panel_width.saturating_sub(header_label.len() + 2)
                );
                let header_line = Line::from(vec![
                    Span::styled("─", Style::default().fg(rgb(theme.border))),
                    Span::styled(
                        header_label,
                        Style::default().fg(rgb(theme.accent)).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(dashes_after, Style::default().fg(rgb(theme.border))),
                ]);

                let mut panel_lines: Vec<Line<'static>> = vec![header_line];
                for (id, type_label, task, elapsed, icon) in agent_rows.iter().take(6) {
                    let icon_style = match *icon {
                        "⟳" => Style::default().fg(rgb(theme.accent)),
                        "✓" => Style::default().fg(rgb(theme.success)),
                        "✗" => Style::default().fg(rgb(theme.error)),
                        _ => Style::default().fg(Color::DarkGray),
                    };
                    let id_str = format!("#{id:02}");
                    let type_str = format!("{type_label:<10}");
                    let elapsed_str = format!("{elapsed:>6}");
                    let fixed_width = 2 + 4 + 2 + 10 + 2 + elapsed_str.len() + 2;
                    let task_width = panel_width.saturating_sub(fixed_width);
                    let task_truncated = if task.chars().count() > task_width {
                        let t: String = task.chars().take(task_width.saturating_sub(1)).collect();
                        format!("{t}…")
                    } else {
                        format!("{task:<task_width$}")
                    };
                    panel_lines.push(Line::from(vec![
                        Span::styled(format!(" {icon} "), icon_style),
                        Span::styled(
                            format!("{id_str}  "),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            type_str,
                            Style::default().fg(rgb(theme.text_secondary)),
                        ),
                        Span::styled(
                            format!("  {task_truncated}"),
                            Style::default().fg(rgb(theme.text_primary)),
                        ),
                        Span::styled(
                            format!("  {elapsed_str} "),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }

                panel_lines.push(Line::from(Span::styled(
                    "─".repeat(panel_width),
                    Style::default().fg(rgb(theme.border)),
                )));

                let panel_widget = Paragraph::new(Text::from(panel_lines))
                    .style(Style::default().bg(rgb(theme.bg_primary)));
                frame.render_widget(panel_widget, panel_area);
            }

            // ── footer (dynamic height: 5 + input_line_count lines) ──────────

            // Line 0: separator
            let separator = "─".repeat(width);
            let line0 = Line::from(Span::styled(
                separator,
                Style::default().fg(rgb(theme.border)),
            ));

            // Lines 1..N: input area
            let input_lines_rendered: Vec<Line<'static>> =
                if configure_state == ConfigureState::Inactive {
                    // Render multi-line input with inline block cursor.
                    let prompt_style = Style::default()
                        .fg(rgb(theme.accent))
                        .add_modifier(Modifier::BOLD);
                    let text_style = Style::default().fg(Color::White);
                    let cursor_fg = Color::Rgb(0x1a, 0x1a, 0x1a);
                    let cursor_bg = Color::White;

                    let prompt_width: usize = 2;
                    let first_col = width.saturating_sub(prompt_width).max(1);
                    let rest_col = width.max(1);

                    // Collect visual rows.
                    let mut visual_rows: Vec<Vec<(usize, char)>> = Vec::new();
                    let mut current_row_chars: Vec<(usize, char)> = Vec::new();
                    let mut byte_off: usize = 0;
                    let logical_segs: Vec<&str> = input_text.split('\n').collect();
                    let n_segs = logical_segs.len();

                    for (seg_idx, seg) in logical_segs.iter().enumerate() {
                        if seg_idx > 0 {
                            visual_rows.push(std::mem::take(&mut current_row_chars));
                        }
                        let mut col_in_row: usize = 0;

                        for ch in seg.chars() {
                            let avail_now = if visual_rows.is_empty() { first_col } else { rest_col };
                            if col_in_row >= avail_now {
                                visual_rows.push(std::mem::take(&mut current_row_chars));
                                col_in_row = 0;
                            }
                            current_row_chars.push((byte_off, ch));
                            byte_off += ch.len_utf8();
                            col_in_row += 1;
                        }
                        if seg_idx + 1 < n_segs {
                            byte_off += 1; // '\n'
                        }
                    }
                    visual_rows.push(current_row_chars); // push the last row

                    // Cap at 5 rows.
                    visual_rows.truncate(5);
                    let n_rows = visual_rows.len();

                    let mut lines: Vec<Line<'static>> = Vec::with_capacity(n_rows);
                    for (row_idx, row_chars) in visual_rows.iter().enumerate() {
                        let is_last_row = row_idx + 1 == n_rows;
                        let mut before = String::new();
                        let mut cur_str = String::new();
                        let mut after = String::new();
                        let mut cursor_placed = false;

                        for &(boff, ch) in row_chars {
                            if !cursor_placed && boff == cursor_pos {
                                cur_str.push(ch);
                                cursor_placed = true;
                            } else if boff < cursor_pos {
                                before.push(ch);
                            } else {
                                after.push(ch);
                            }
                        }

                        let trailing_cursor = !cursor_placed
                            && is_last_row
                            && cursor_pos >= input_text.len();

                        let mut spans: Vec<Span<'static>> = Vec::new();
                        if row_idx == 0 {
                            spans.push(Span::styled("❯ ", prompt_style));
                        }
                        if !before.is_empty() {
                            spans.push(Span::styled(before, text_style));
                        }
                        if cursor_placed {
                            spans.push(Span::styled(
                                cur_str,
                                Style::default().fg(cursor_fg).bg(cursor_bg),
                            ));
                            if !after.is_empty() {
                                spans.push(Span::styled(after, text_style));
                            }
                        } else if trailing_cursor {
                            spans.push(Span::styled(
                                " ",
                                Style::default().fg(cursor_fg).bg(cursor_bg),
                            ));
                        } else {
                            if !after.is_empty() {
                                spans.push(Span::styled(after, text_style));
                            }
                            if spans.iter().all(|s| s.content.is_empty()) {
                                spans.push(Span::raw(" "));
                            }
                        }
                        lines.push(Line::from(spans));
                    }
                    lines
                } else {
                    let breadcrumb = configure_breadcrumb(&configure_state);
                    vec![Line::from(vec![
                        Span::styled(
                            "⚒ ",
                            Style::default()
                                .fg(rgb(theme.accent))
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            breadcrumb,
                            Style::default()
                                .fg(rgb(theme.accent))
                                .add_modifier(Modifier::DIM),
                        ),
                        Span::styled(
                            "   ↑↓ Navigate  Enter Select  Esc Back",
                            Style::default().fg(rgb(theme.border)),
                        ),
                    ])]
                };

            // Blank line after input.
            let line_blank = Line::from("");

            // Line 3: Model: {name} | Total: {n}M | Cost: ${x} | ⌐{branch} | (+x,-y)    {total} tokens
            let total_tokens = input_tokens.saturating_add(output_tokens);
            let total_m = f64::from(total_tokens) / 1_000_000.0;
            let cost_usd = {
                if model.contains(':') && !model.contains(":cloud") {
                    "local".to_string()
                } else if let Some(p) = pricing_for_model(&model) {
                    let cost = (f64::from(input_tokens) / 1_000_000.0) * p.input_cost_per_million
                        + (f64::from(output_tokens) / 1_000_000.0) * p.output_cost_per_million;
                    format_usd(cost)
                } else if model.contains(':') {
                    "cloud".to_string()
                } else {
                    format_usd(0.0)
                }
            };
            let right3 = format!("{total_tokens} tokens");
            let line3 = build_left_right_line(
                build_status1_spans(&model, total_m, &git_branch, &git_diff_stats, &cost_usd),
                vec![Span::styled(
                    right3,
                    Style::default().fg(Color::Rgb(0x88, 0x88, 0x88)),
                )],
                width,
            );

            // Line 4: Context bar | session % | block dur    version
            let used_tokens = input_tokens;
            let bar_width: usize = 16;
            let pct = if context_max_tokens > 0 {
                ((f64::from(used_tokens) / f64::from(context_max_tokens)) * 100.0).min(100.0)
            } else {
                0.0
            };
            let filled = ((pct / 100.0) * bar_width as f64).round() as usize;
            let empty = bar_width.saturating_sub(filled);
            let bar_filled = "█".repeat(filled);
            let bar_empty = "░".repeat(empty);
            let used_k = used_tokens / 1000;
            let max_k = context_max_tokens / 1000;
            let session_pct = if context_max_tokens > 0 { pct } else { 0.0 };
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
                    Style::default().fg(rgb(theme.warning)),
                ),
                Span::styled(
                    perm_display,
                    Style::default()
                        .fg(rgb(theme.warning))
                        .add_modifier(Modifier::DIM),
                ),
            ];
            if !qmd_status.is_empty() {
                line5_left.push(Span::styled(
                    format!("  │  📚 {qmd_status}"),
                    Style::default().fg(Color::Rgb(0x55, 0x88, 0x55)),
                ));
            }
            if !last_archive_status.is_empty() {
                line5_left.push(Span::styled(
                    format!("  │  📦 {last_archive_status}"),
                    Style::default().fg(Color::Rgb(0x55, 0x77, 0xAA)),
                ));
            }
            if !update_available.is_empty() {
                line5_left.push(Span::styled(
                    format!("  │  ⬆ {update_available}"),
                    Style::default()
                        .fg(Color::Rgb(0xFF, 0xAA, 0x00))
                        .add_modifier(Modifier::BOLD),
                ));
            }
            let line5 = Line::from(line5_left);

            // Assemble footer.
            let mut footer_lines: Vec<Line<'static>> = Vec::new();
            footer_lines.push(line0);
            footer_lines.extend(input_lines_rendered);
            footer_lines.push(line_blank);
            footer_lines.push(line3);
            footer_lines.push(line4);
            footer_lines.push(line5);
            let footer_widget = Paragraph::new(Text::from(footer_lines));
            frame.render_widget(footer_widget, footer_area);

            // ─── Completion popup overlay ────────────────────────────────
            if completion_visible && !completion_matches.is_empty() {
                let popup_height = (completion_matches.len() as u16).min(12) + 2;
                let popup_width = (width as u16).min(60);
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
                            (rgb(theme.bg_primary), rgb(theme.accent))
                        } else {
                            (rgb(theme.text_primary), rgb(theme.bg_card))
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
                                    .fg(if is_selected { rgb(theme.bg_primary) } else { rgb(theme.text_secondary) })
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
                            .border_style(Style::default().fg(rgb(theme.border)))
                            .style(Style::default().bg(rgb(theme.bg_card))),
                    );
                frame.render_widget(ratatui::widgets::Clear, popup_area);
                frame.render_widget(popup_widget, popup_area);
            }

            // Position the terminal cursor within the (possibly multi-row) input area.
            let (cursor_row_offset, cursor_col) =
                cursor_visual_position(&input_text, cursor_pos, width);
            let cursor_x = footer_area.x + cursor_col as u16;
            let cursor_y = footer_area.y + 1 + cursor_row_offset as u16;
            let max_x = footer_area.x + footer_area.width.saturating_sub(1);
            frame.set_cursor_position(Position {
                x: cursor_x.min(max_x),
                y: cursor_y,
            });
        })?;
        Ok(())
    }

    // ─── Event processing ────────────────────────────────────────────────────

    /// Flush any pending streaming text into the log as a completed assistant message.
    pub(super) fn flush_pending_text(&mut self) {
        let text = std::mem::take(&mut self.active_tab_mut().pending_text);
        if !text.trim().is_empty() {
            self.active_tab_mut().log.push(LogEntry::Assistant(text));
        }
    }

    /// Drain all queued TUI events without blocking.
    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.apply_tui_event(event);
        }
    }

    fn apply_tui_event(&mut self, event: TuiEvent) {
        match event {
            TuiEvent::TextDelta(text) => {
                self.active_tab_mut().pending_text.push_str(&text);
            }
            TuiEvent::TextDone | TuiEvent::TurnDone => {
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
        }
    }

    /// Auto-scroll to the bottom of the content.
    pub(super) fn scroll_to_bottom(&mut self) {
        self.active_tab_mut().scroll = usize::MAX;
    }

    /// Scroll up by `n` lines.
    pub fn scroll_up(&mut self, n: usize) {
        let s = self.active_tab_mut().scroll;
        self.active_tab_mut().scroll = s.saturating_sub(n);
    }

    /// Scroll down by `n` lines (`draw()` clamps to max).
    pub fn scroll_down(&mut self, n: usize) {
        let s = self.active_tab_mut().scroll;
        self.active_tab_mut().scroll = s.saturating_add(n);
    }

    // ─── Public interface ────────────────────────────────────────────────────

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

    /// Set the update notification message shown in the footer.
    pub fn set_update_available(&mut self, msg: impl Into<String>) {
        self.update_available = msg.into();
    }

    // ─── Agent panel ─────────────────────────────────────────────────────────

    /// Refresh the agent panel rows snapshot from the caller's `AgentManager`.
    ///
    /// Each tuple: `(id, type_label, task, elapsed_str, status_icon)`.
    pub fn update_agent_rows(
        &mut self,
        rows: Vec<(usize, String, String, String, &'static str)>,
    ) {
        self.agent_rows = rows;
        if !self.agent_rows.is_empty() {
            self.agent_panel_visible = true;
        }
    }

    /// Toggle the agent panel visibility.
    #[allow(dead_code)]
    pub fn toggle_agent_panel(&mut self) {
        self.agent_panel_visible = !self.agent_panel_visible;
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

    // ─── Configure mode ──────────────────────────────────────────────────────

    /// Enter the interactive configure menu.  Call this when the user runs
    /// `/configure` in the TUI.  `data` is a snapshot of live config values
    /// built by the caller (`LiveCli`).
    pub fn enter_configure_mode(&mut self, data: ConfigureData) {
        self.configure_data = data;
        self.configure_state = ConfigureState::MainMenu { selected: 0 };
    }

    /// Key handler for configure mode.
    pub(super) fn handle_configure_key(&mut self, key: crossterm::event::KeyEvent) -> io::Result<ReadResult> {
        use configure_types::ConfigureState;

        // EditingValue has its own character-level handler.
        if let ConfigureState::EditingValue { ref mut value, ref mut cursor, .. } =
            self.configure_state
        {
            match key.code {
                KeyCode::Esc => {
                    let section = match &self.configure_state {
                        ConfigureState::EditingValue { section, .. } => section.clone(),
                        _ => String::new(),
                    };
                    self.configure_state = section_state_from_name(&section, 0);
                    return Ok(ReadResult::Continue);
                }
                KeyCode::Enter => {
                    let (section, key_name, value_str) = match &self.configure_state {
                        ConfigureState::EditingValue { section, key, value, .. } => {
                            (section.clone(), key.clone(), value.clone())
                        }
                        _ => unreachable!(),
                    };
                    let action = configure_action_for(&section, &key_name, &value_str);
                    self.configure_state = section_state_from_name(&section, 0);
                    if let Some(action) = action {
                        return Ok(ReadResult::ConfigureAction(action));
                    }
                    return Ok(ReadResult::Continue);
                }
                KeyCode::Char(ch) => {
                    value.insert(*cursor, ch);
                    *cursor += ch.len_utf8();
                }
                KeyCode::Backspace => {
                    if *cursor > 0 {
                        let prev = prev_char_boundary(value, *cursor);
                        value.drain(prev..*cursor);
                        *cursor = prev;
                    }
                }
                KeyCode::Delete => {
                    if *cursor < value.len() {
                        let next = next_char_boundary(value, *cursor);
                        value.drain(*cursor..next);
                    }
                }
                KeyCode::Left => {
                    if *cursor > 0 {
                        *cursor = prev_char_boundary(value, *cursor);
                    }
                }
                KeyCode::Right => {
                    if *cursor < value.len() {
                        *cursor = next_char_boundary(value, *cursor);
                    }
                }
                KeyCode::Home => *cursor = 0,
                KeyCode::End => *cursor = value.len(),
                _ => {}
            }
            return Ok(ReadResult::Continue);
        }

        // Navigation / selection for all other states.
        match key.code {
            KeyCode::Up => {
                self.configure_move(-1);
            }
            KeyCode::Down => {
                self.configure_move(1);
            }
            KeyCode::Enter => {
                return self.configure_select();
            }
            KeyCode::Esc => {
                self.configure_back();
            }
            _ => {}
        }
        Ok(ReadResult::Continue)
    }

    /// Move the selected item index by `delta` (-1 = up, +1 = down).
    #[allow(clippy::cast_sign_loss)]
    fn configure_move(&mut self, delta: i32) {
        let count = configure_item_count(&self.configure_state, &self.configure_data);
        if count == 0 {
            return;
        }
        let selected = configure_selected(&self.configure_state);
        let new_selected = if delta < 0 {
            selected.saturating_sub((-delta) as usize)
        } else {
            (selected + delta as usize).min(count - 1)
        };
        configure_set_selected(&mut self.configure_state, new_selected);
    }

    /// Handle Enter on the currently selected item.
    fn configure_select(&mut self) -> io::Result<ReadResult> {
        let selected = configure_selected(&self.configure_state);
        match self.configure_state.clone() {
            ConfigureState::MainMenu { .. } => {
                self.configure_state = match selected {
                    0 => ConfigureState::Providers { selected: 0 },
                    1 => ConfigureState::Models { selected: 0 },
                    2 => ConfigureState::Context { selected: 0 },
                    3 => ConfigureState::Search { selected: 0 },
                    4 => ConfigureState::Permissions { selected: 0 },
                    5 => ConfigureState::Display { selected: 0 },
                    6 => ConfigureState::Integrations { selected: 0 },
                    7 => ConfigureState::LanguageTheme { selected: 0 },
                    8 => ConfigureState::Vault { selected: 0 },
                    9 => ConfigureState::Notifications { selected: 0 },
                    10 => ConfigureState::Failover { selected: 0 },
                    11 => ConfigureState::Ssh { selected: 0 },
                    12 => ConfigureState::DockerK8s { selected: 0 },
                    13 => ConfigureState::Database { selected: 0 },
                    14 => ConfigureState::MemoryArchive { selected: 0 },
                    15 => ConfigureState::PluginsCron { selected: 0 },
                    _ => ConfigureState::MainMenu { selected },
                };
            }
            ConfigureState::Providers { .. } => {
                let provider = match selected {
                    0 => "anthropic",
                    1 => "openai",
                    2 => "ollama",
                    3 => "xai",
                    _ => return Ok(ReadResult::Continue),
                };
                self.configure_state = ConfigureState::ProviderDetail {
                    provider: provider.to_string(),
                    selected: 0,
                };
            }
            ConfigureState::ProviderDetail { ref provider, selected } => {
                let p = provider.clone();
                match (p.as_str(), selected) {
                    ("anthropic", 0) => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(
                            ConfigureAction::RefreshAnthropicOAuth,
                        ));
                    }
                    ("anthropic", 1) | ("openai" | "xai", 0) => {
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Providers".to_string(),
                            key: format!("{p}_api_key"),
                            value: String::new(),
                            cursor: 0,
                        };
                    }
                    ("ollama", 0) => {
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Providers".to_string(),
                            key: "ollama_host".to_string(),
                            value: self.configure_data.ollama_host.clone(),
                            cursor: self.configure_data.ollama_host.len(),
                        };
                    }
                    _ => {}
                }
            }
            ConfigureState::Models { selected } => {
                match selected {
                    0 => {
                        let current = self.configure_data.default_model.clone();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Models".to_string(),
                            key: "default_model".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    1 => {
                        let current = self.configure_data.image_model.clone();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Models".to_string(),
                            key: "image_model".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    _ => {}
                }
            }
            ConfigureState::Context { selected } => {
                match selected {
                    0 => {
                        let current = self.configure_data.context_size.to_string();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Context".to_string(),
                            key: "context_size".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    1 => {
                        let current = self.configure_data.compact_threshold.to_string();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Context".to_string(),
                            key: "compact_threshold".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    2 => {
                        self.configure_state = ConfigureState::Inactive;
                        let enabled = !self.configure_data.qmd_status.contains("disabled");
                        return Ok(ReadResult::ConfigureAction(
                            ConfigureAction::SetQmdEnabled { enabled: !enabled },
                        ));
                    }
                    _ => {}
                }
            }
            ConfigureState::Search { selected } => {
                match selected {
                    0 => {
                        let current = self.configure_data.default_search.clone();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Search".to_string(),
                            key: "default_search".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    n if n >= 1 && n <= self.configure_data.search_providers.len() => {
                        let provider_name =
                            self.configure_data.search_providers[n - 1].0.clone();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Search".to_string(),
                            key: format!("{provider_name}_key"),
                            value: String::new(),
                            cursor: 0,
                        };
                    }
                    _ => {}
                }
            }
            ConfigureState::Permissions { selected } => {
                let mode = match selected {
                    0 => "read-only",
                    1 => "workspace-write",
                    2 => "danger-full-access",
                    _ => return Ok(ReadResult::Continue),
                };
                self.configure_state = ConfigureState::Inactive;
                return Ok(ReadResult::ConfigureAction(
                    ConfigureAction::SetPermissionMode { mode: mode.to_string() },
                ));
            }
            ConfigureState::Display { selected } => {
                match selected {
                    0 => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::ToggleVim));
                    }
                    1 => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::ToggleChat));
                    }
                    _ => {}
                }
            }
            ConfigureState::LanguageTheme { selected } => {
                match selected {
                    0 => {
                        let langs = ["en", "de", "es", "fr", "ja", "zh-CN", "ru"];
                        let current = &self.configure_data.language;
                        let idx = langs.iter().position(|l| *l == current.as_str()).unwrap_or(0);
                        let next = langs[(idx + 1) % langs.len()];
                        self.configure_data.language = next.to_string();
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::SetLanguage {
                            lang: next.to_string(),
                        }));
                    }
                    1 => {
                        let themes = [
                            "culpur-defense", "cyberpunk", "nord", "solarized-dark",
                            "dracula", "monokai", "gruvbox", "catppuccin",
                        ];
                        let current = &self.configure_data.active_theme;
                        let idx = themes.iter().position(|t| *t == current.as_str()).unwrap_or(0);
                        let next = themes[(idx + 1) % themes.len()];
                        self.configure_data.active_theme = next.to_string();
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::SetTheme {
                            theme: next.to_string(),
                        }));
                    }
                    _ => {}
                }
            }
            ConfigureState::Vault { selected } => {
                match selected {
                    0 => {
                        let current = self.configure_data.vault_session_ttl.to_string();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Vault".to_string(),
                            key: "vault_session_ttl".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    1 => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::ToggleVaultAutoLock));
                    }
                    _ => {}
                }
            }
            ConfigureState::Notifications { selected } => {
                let key = match selected {
                    0 => {
                        let platforms = [
                            "desktop", "discord", "slack", "telegram",
                            "whatsapp", "signal", "matrix", "webhook",
                        ];
                        let current = &self.configure_data.notify_platform;
                        let idx = platforms.iter().position(|p| *p == current.as_str()).unwrap_or(0);
                        let next = platforms[(idx + 1) % platforms.len()];
                        self.configure_data.notify_platform = next.to_string();
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::SetNotifyPlatform {
                            platform: next.to_string(),
                        }));
                    }
                    1 => "notify_discord_webhook",
                    2 => "notify_slack_webhook",
                    3 => "notify_telegram_token",
                    4 => "notify_whatsapp_url",
                    5 => "notify_whatsapp_token",
                    6 => "notify_matrix_homeserver",
                    7 => "notify_matrix_token",
                    8 => "notify_signal_sender",
                    9 => "notify_signal_cli_path",
                    _ => return Ok(ReadResult::Continue),
                };
                let current_val = configure_data_notify_value(&self.configure_data, key);
                let cur_len = current_val.len();
                self.configure_state = ConfigureState::EditingValue {
                    section: "Notifications".to_string(),
                    key: key.to_string(),
                    value: current_val,
                    cursor: cur_len,
                };
            }
            ConfigureState::Failover { selected } => {
                match selected {
                    0 => {
                        let current = self.configure_data.failover_cooldown.to_string();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Failover".to_string(),
                            key: "failover_cooldown".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    1 => {
                        let current = self.configure_data.failover_budget.to_string();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Failover".to_string(),
                            key: "failover_budget".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    2 => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::ToggleFailoverAutoRecovery));
                    }
                    _ => {}
                }
            }
            ConfigureState::Ssh { selected } => {
                let (key, cur_val) = match selected {
                    0 => ("ssh_key_path", self.configure_data.ssh_key_path.clone()),
                    1 => ("ssh_bastion_host", self.configure_data.ssh_bastion_host.clone()),
                    2 => ("ssh_config_path", self.configure_data.ssh_config_path.clone()),
                    _ => return Ok(ReadResult::Continue),
                };
                let cur_len = cur_val.len();
                self.configure_state = ConfigureState::EditingValue {
                    section: "SSH".to_string(),
                    key: key.to_string(),
                    value: cur_val,
                    cursor: cur_len,
                };
            }
            ConfigureState::DockerK8s { selected } => {
                let (key, cur_val) = match selected {
                    0 => ("docker_compose_file", self.configure_data.docker_compose_file.clone()),
                    1 => ("docker_registry", self.configure_data.docker_registry.clone()),
                    2 => ("k8s_context", self.configure_data.k8s_context.clone()),
                    3 => ("k8s_namespace", self.configure_data.k8s_namespace.clone()),
                    _ => return Ok(ReadResult::Continue),
                };
                let cur_len = cur_val.len();
                self.configure_state = ConfigureState::EditingValue {
                    section: "DockerK8s".to_string(),
                    key: key.to_string(),
                    value: cur_val,
                    cursor: cur_len,
                };
            }
            ConfigureState::Database { selected } => {
                match selected {
                    0 => {
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Database".to_string(),
                            key: "db_url".to_string(),
                            value: String::new(),
                            cursor: 0,
                        };
                    }
                    1 => {
                        let tools = ["prisma", "knex", "typeorm"];
                        let current = &self.configure_data.db_schema_tool;
                        let idx = tools.iter().position(|t| *t == current.as_str()).unwrap_or(0);
                        let next = tools[(idx + 1) % tools.len()];
                        self.configure_data.db_schema_tool = next.to_string();
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::SetDbSchemaTool {
                            tool: next.to_string(),
                        }));
                    }
                    _ => {}
                }
            }
            ConfigureState::MemoryArchive { selected } => {
                match selected {
                    0 => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::ToggleAutoSaveMemory));
                    }
                    1 => {
                        let current = self.configure_data.archive_frequency.to_string();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "MemoryArchive".to_string(),
                            key: "archive_frequency".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    2 => {
                        let current = self.configure_data.archive_retention_days.to_string();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "MemoryArchive".to_string(),
                            key: "archive_retention_days".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    3 => {
                        let current = self.configure_data.memory_dir.clone();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "MemoryArchive".to_string(),
                            key: "memory_dir".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    _ => {}
                }
            }
            ConfigureState::PluginsCron { selected } => {
                match selected {
                    0 => {
                        let current = self.configure_data.plugin_search_paths.clone();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "PluginsCron".to_string(),
                            key: "plugin_search_paths".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    1 => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::ToggleAutoEnablePlugins));
                    }
                    2 => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::ToggleCronEnabled));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(ReadResult::Continue)
    }

    /// Go back one level in the configure hierarchy.
    fn configure_back(&mut self) {
        self.configure_state = match &self.configure_state {
            ConfigureState::MainMenu { .. } | ConfigureState::Inactive => ConfigureState::Inactive,
            ConfigureState::Providers { .. }
            | ConfigureState::Models { .. }
            | ConfigureState::Context { .. }
            | ConfigureState::Search { .. }
            | ConfigureState::Permissions { .. }
            | ConfigureState::Display { .. }
            | ConfigureState::Integrations { .. }
            | ConfigureState::LanguageTheme { .. }
            | ConfigureState::Vault { .. }
            | ConfigureState::Notifications { .. }
            | ConfigureState::Failover { .. }
            | ConfigureState::Ssh { .. }
            | ConfigureState::DockerK8s { .. }
            | ConfigureState::Database { .. }
            | ConfigureState::MemoryArchive { .. }
            | ConfigureState::PluginsCron { .. } => ConfigureState::MainMenu { selected: 0 },
            ConfigureState::ProviderDetail { .. } => ConfigureState::Providers { selected: 0 },
            ConfigureState::EditingValue { section, .. } => {
                section_state_from_name(section, 0)
            }
        };
    }

    // ─── Screensaver integration ──────────────────────────────────────────────

    /// Capture a flat list of printable lines currently shown in the content area.
    pub fn capture_screen_text(&self) -> Vec<String> {
        let tab = &self.tabs[self.active_tab];
        let mut out: Vec<String> = Vec::new();
        for entry in &tab.log {
            match entry {
                LogEntry::User(t) | LogEntry::Assistant(t) | LogEntry::System(t) => {
                    for line in t.lines() {
                        out.push(line.to_string());
                    }
                }
                LogEntry::ToolCall { name, detail, .. } => {
                    out.push(format!("  {name}: {}", detail.lines().next().unwrap_or("")));
                }
            }
        }
        for line in tab.pending_text.lines() {
            out.push(line.to_string());
        }
        out
    }

    /// Render one screensaver frame.
    pub fn draw_screensaver(&mut self, ss: &mut crate::screensaver::FurnaceScreensaver) -> io::Result<bool> {
        let (w, h) = {
            let size = self.terminal.size()?;
            (size.width, size.height)
        };
        let still_active = ss.tick(w, h);

        self.terminal.draw(|frame| {
            ss.render(frame, frame.area());
        })?;

        Ok(still_active)
    }

    /// Read a single input event in screensaver mode.
    pub fn read_input_screensaver(&mut self, ss: &mut crate::screensaver::FurnaceScreensaver) -> io::Result<ReadResult> {
        let still_active = self.draw_screensaver(ss)?;
        if !still_active {
            return Ok(ReadResult::Continue);
        }

        if event::poll(crate::screensaver::FRAME_INTERVAL)? {
            match event::read()? {
                CtEvent::Key(key) if matches!(key.kind, KeyEventKind::Press) => {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c' | 'C' | 'd' | 'D'))
                    {
                        return Ok(ReadResult::Exit);
                    }
                    ss.resume();
                }
                CtEvent::Mouse(_) => {
                    ss.resume();
                }
                _ => {}
            }
        }
        Ok(ReadResult::Continue)
    }
}

impl Drop for AnvilTui {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            io::stdout(),
            crossterm::event::DisableBracketedPaste,
            crossterm::event::DisableMouseCapture,
            terminal::LeaveAlternateScreen
        );
    }
}

// ─── Configure menu renderer ─────────────────────────────────────────────────

/// Render the configure menu for the current state as a Vec of ratatui Lines.
fn render_configure_menu(
    state: &ConfigureState,
    data: &ConfigureData,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    let make_row = |label: &str, value: &str, is_cursor: bool| -> Line<'static> {
        let marker = if is_cursor { "  ▸ " } else { "    " };
        let label_str = label.to_string();
        let value_str = value.to_string();
        let label_padded = format!("{label_str:<40}");
        if is_cursor {
            Line::from(vec![
                Span::styled(marker.to_string(), Style::default().fg(Color::Cyan)),
                Span::styled(
                    label_padded,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    value_str,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                ),
            ])
        } else {
            Line::from(vec![
                Span::raw(marker.to_string()),
                Span::styled(label_padded, Style::default().fg(Color::Rgb(0xaa, 0xaa, 0xaa))),
                Span::styled(
                    value_str,
                    Style::default().fg(Color::Rgb(0x66, 0x66, 0x88)).add_modifier(Modifier::DIM),
                ),
            ])
        }
    };

    let heading = configure_breadcrumb(state);
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("  ⚒  {heading}"),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        format!("  {}", "─".repeat(width.saturating_sub(4))),
        Style::default().fg(Color::Rgb(0x33, 0x33, 0x44)),
    )));
    lines.push(Line::from(""));

    let sel = configure_selected(state);

    match state {
        ConfigureState::Inactive => {}

        ConfigureState::MainMenu { .. } => {
            let items: &[(&str, String)] = &[
                ("Providers & Authentication", format!("[{}]", {
                    let mut n = 0;
                    if !data.anthropic_status.contains('✗') { n += 1; }
                    if !data.openai_status.contains('✗') { n += 1; }
                    if !data.ollama_status.contains('✗') { n += 1; }
                    if !data.xai_status.contains('✗') { n += 1; }
                    format!("{n} configured")
                })),
                ("Models & Defaults", format!("[{}]", data.current_model)),
                ("Context & Memory", format!("[{}K]", data.context_size / 1000)),
                ("Search Providers", format!("[{}]", data.default_search)),
                ("Permissions", format!("[{}]", data.permission_mode)),
                ("Display & Interface", format!("[vim:{}]", if data.vim_mode { "on" } else { "off" })),
                ("Integrations", format!("[AnvilHub {}]", if data.anvilhub_url.is_empty() { "✗" } else { "✓" })),
                ("Language & Theme", format!("[{} / {}]", data.language, data.active_theme)),
                ("Vault", {
                    let ttl = data.vault_session_ttl;
                    format!("[TTL {}s, auto-lock:{}]", ttl, if data.vault_auto_lock { "on" } else { "off" })
                }),
                ("Notifications", format!("[{}]", data.notify_platform)),
                ("Failover", format!("[cooldown {}s, auto-recovery:{}]", data.failover_cooldown, if data.failover_auto_recovery { "on" } else { "off" })),
                ("SSH", {
                    if data.ssh_bastion_host.is_empty() {
                        "[not configured]".to_string()
                    } else {
                        format!("[bastion: {}]", data.ssh_bastion_host)
                    }
                }),
                ("Docker & K8s", {
                    if data.k8s_context.is_empty() {
                        "[not configured]".to_string()
                    } else {
                        format!("[ctx: {}]", data.k8s_context)
                    }
                }),
                ("Database", {
                    if data.db_url.is_empty() {
                        "[not configured]".to_string()
                    } else {
                        format!("[{} / {}]", data.db_schema_tool, mask_sensitive(&data.db_url))
                    }
                }),
                ("Memory & Archive", format!("[auto-save:{}, retention:{}d]", if data.auto_save_memory { "on" } else { "off" }, data.archive_retention_days)),
                ("Plugins & Cron", format!("[cron:{}, {} active jobs]", if data.cron_enabled { "on" } else { "off" }, data.active_cron_jobs.len())),
            ];
            for (i, (label, value)) in items.iter().enumerate() {
                lines.push(make_row(label, value, i == sel));
            }
        }

        ConfigureState::Providers { .. } => {
            let items = [
                ("Anthropic", data.anthropic_status.clone()),
                ("OpenAI", data.openai_status.clone()),
                ("Ollama", format!("{} ({})", data.ollama_status, data.ollama_host)),
                ("xAI", data.xai_status.clone()),
            ];
            for (i, (label, value)) in items.iter().enumerate() {
                lines.push(make_row(label, value, i == sel));
            }
        }

        ConfigureState::ProviderDetail { provider, .. } => {
            match provider.as_str() {
                "anthropic" => {
                    lines.push(Line::from(vec![
                        Span::raw("    Status:  "),
                        Span::styled(data.anthropic_status.clone(), Style::default().fg(Color::Cyan)),
                    ]));
                    lines.push(Line::from(""));
                    lines.push(make_row("Refresh OAuth token", "[opens browser]", sel == 0));
                    lines.push(make_row("Set API key instead", "[enter key]", sel == 1));
                }
                "openai" => {
                    lines.push(Line::from(vec![
                        Span::raw("    Status:  "),
                        Span::styled(data.openai_status.clone(), Style::default().fg(Color::Cyan)),
                    ]));
                    lines.push(Line::from(""));
                    lines.push(make_row("Set OPENAI_API_KEY", "[enter key]", sel == 0));
                }
                "ollama" => {
                    lines.push(Line::from(vec![
                        Span::raw("    Status:  "),
                        Span::styled(data.ollama_status.clone(), Style::default().fg(Color::Cyan)),
                    ]));
                    lines.push(Line::from(vec![
                        Span::raw("    Host:    "),
                        Span::styled(data.ollama_host.clone(), Style::default().fg(Color::Yellow)),
                    ]));
                    lines.push(Line::from(""));
                    lines.push(make_row("Set Ollama host URL", "[edit]", sel == 0));
                }
                "xai" => {
                    lines.push(Line::from(vec![
                        Span::raw("    Status:  "),
                        Span::styled(data.xai_status.clone(), Style::default().fg(Color::Cyan)),
                    ]));
                    lines.push(Line::from(""));
                    lines.push(make_row("Set XAI_API_KEY", "[enter key]", sel == 0));
                }
                _ => {}
            }
        }

        ConfigureState::Models { .. } => {
            lines.push(make_row("Default model", &data.default_model, sel == 0));
            lines.push(make_row("Image model", &data.image_model, sel == 1));
            if !data.failover_chain.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "    Failover chain:",
                    Style::default().fg(Color::Rgb(0x88, 0x88, 0x88)),
                )));
                for (i, m) in data.failover_chain.iter().enumerate() {
                    lines.push(Line::from(Span::styled(
                        format!("      {}. {m}", i + 1),
                        Style::default().fg(Color::Rgb(0x66, 0x88, 0x66)),
                    )));
                }
            }
        }

        ConfigureState::Context { .. } => {
            let size_label = {
                let kb = data.context_size / 1000;
                let mb = data.context_size / 1_000_000;
                if mb > 0 { format!("{mb}M tokens") } else { format!("{kb}K tokens") }
            };
            lines.push(make_row("Context window size", &size_label, sel == 0));
            lines.push(make_row(
                "Auto-compact threshold",
                &format!("{}%", data.compact_threshold),
                sel == 1,
            ));
            let qmd_label = if data.qmd_status.contains("disabled") { "off" } else { "on" };
            lines.push(make_row(
                "QMD integration",
                &format!("{qmd_label}  ({}) ", data.qmd_status),
                sel == 2,
            ));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("    History archives: {}  |  Pinned files: {}", data.history_count, data.pinned_count),
                Style::default().fg(Color::Rgb(0x66, 0x66, 0x88)),
            )));
        }

        ConfigureState::Search { .. } => {
            lines.push(make_row("Default search provider", &data.default_search, sel == 0));
            for (i, (name, enabled, has_key)) in data.search_providers.iter().enumerate() {
                let status = if *has_key { "✓ key set" } else { "✗ no key" };
                let enabled_str = if *enabled { "" } else { " [disabled]" };
                lines.push(make_row(
                    name.as_str(),
                    &format!("{status}{enabled_str}"),
                    sel == i + 1,
                ));
            }
        }

        ConfigureState::Permissions { .. } => {
            let current = &data.permission_mode;
            let modes = [
                ("read-only", "Read-only — no file writes"),
                ("workspace-write", "Workspace write — within project"),
                ("danger-full-access", "Full access — no restrictions"),
            ];
            for (i, (mode, desc)) in modes.iter().enumerate() {
                let active = if current == *mode { " [active]" } else { "" };
                lines.push(make_row(&format!("{desc}{active}"), "", i == sel));
            }
        }

        ConfigureState::Display { .. } => {
            lines.push(make_row(
                "Vim keybindings",
                if data.vim_mode { "[on]  — Enter to toggle" } else { "[off] — Enter to toggle" },
                sel == 0,
            ));
            lines.push(make_row(
                "Chat-only mode (no tools)",
                if data.chat_mode { "[on]  — Enter to toggle" } else { "[off] — Enter to toggle" },
                sel == 1,
            ));
        }

        ConfigureState::Integrations { .. } => {
            let hub = if data.anvilhub_url.is_empty() {
                "✗ not configured".to_string()
            } else {
                format!("✓ {}", data.anvilhub_url)
            };
            lines.push(make_row("AnvilHub", &hub, sel == 0));
            lines.push(make_row(
                "WordPress",
                if data.wp_configured { "✓ configured" } else { "✗ not configured" },
                sel == 1,
            ));
            lines.push(make_row(
                "GitHub",
                if data.github_configured { "✓ configured" } else { "✗ not configured" },
                sel == 2,
            ));
        }

        ConfigureState::LanguageTheme { .. } => {
            lines.push(make_row(
                "Display language",
                &format!("[{}]  — Enter to cycle", data.language),
                sel == 0,
            ));
            lines.push(make_row(
                "Active theme",
                &format!("[{}]  — Enter to cycle", data.active_theme),
                sel == 1,
            ));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "    Languages: en  de  es  fr  ja  zh-CN  ru",
                Style::default().fg(Color::Rgb(0x66, 0x66, 0x88)),
            )));
            lines.push(Line::from(Span::styled(
                "    Themes: culpur-defense  cyberpunk  nord  solarized-dark  dracula  monokai  gruvbox  catppuccin",
                Style::default().fg(Color::Rgb(0x66, 0x66, 0x88)),
            )));
        }

        ConfigureState::Vault { .. } => {
            lines.push(make_row(
                "Session TTL (seconds)",
                &data.vault_session_ttl.to_string(),
                sel == 0,
            ));
            lines.push(make_row(
                "Auto-lock on idle",
                if data.vault_auto_lock { "[on]  — Enter to toggle" } else { "[off] — Enter to toggle" },
                sel == 1,
            ));
            lines.push(make_row("Vault status", &data.vault_status, sel == 2));
        }

        ConfigureState::Notifications { .. } => {
            lines.push(make_row(
                "Default platform",
                &format!("[{}]  — Enter to cycle", data.notify_platform),
                sel == 0,
            ));
            let masked_or_empty = |s: &str| {
                if s.is_empty() { "[not set]".to_string() } else { mask_sensitive(s) }
            };
            lines.push(make_row("Discord webhook URL",     &masked_or_empty(&data.notify_discord_webhook),   sel == 1));
            lines.push(make_row("Slack webhook URL",       &masked_or_empty(&data.notify_slack_webhook),     sel == 2));
            lines.push(make_row("Telegram bot token",      &masked_or_empty(&data.notify_telegram_token),    sel == 3));
            lines.push(make_row("WhatsApp API URL",        &masked_or_empty(&data.notify_whatsapp_url),      sel == 4));
            lines.push(make_row("WhatsApp token",          &masked_or_empty(&data.notify_whatsapp_token),    sel == 5));
            lines.push(make_row("Matrix homeserver URL",   &masked_or_empty(&data.notify_matrix_homeserver), sel == 6));
            lines.push(make_row("Matrix token",            &masked_or_empty(&data.notify_matrix_token),      sel == 7));
            lines.push(make_row("Signal sender number",    &masked_or_empty(&data.notify_signal_sender),     sel == 8));
            lines.push(make_row("Signal CLI path",         &masked_or_empty(&data.notify_signal_cli_path),   sel == 9));
        }

        ConfigureState::Failover { .. } => {
            lines.push(make_row(
                "Cooldown period (seconds)",
                &data.failover_cooldown.to_string(),
                sel == 0,
            ));
            lines.push(make_row(
                "Usage budget per provider",
                &data.failover_budget.to_string(),
                sel == 1,
            ));
            lines.push(make_row(
                "Auto-recovery",
                if data.failover_auto_recovery { "[on]  — Enter to toggle" } else { "[off] — Enter to toggle" },
                sel == 2,
            ));
            if !data.failover_chain.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "    Provider priority chain:",
                    Style::default().fg(Color::Rgb(0x88, 0x88, 0x88)),
                )));
                for (i, m) in data.failover_chain.iter().enumerate() {
                    lines.push(Line::from(Span::styled(
                        format!("      {}. {m}", i + 1),
                        Style::default().fg(Color::Rgb(0x66, 0x88, 0x66)),
                    )));
                }
            }
        }

        ConfigureState::Ssh { .. } => {
            let fmt = |s: &str| if s.is_empty() { "[not set]".to_string() } else { s.to_string() };
            lines.push(make_row("Default SSH key path",  &fmt(&data.ssh_key_path),     sel == 0));
            lines.push(make_row("Default bastion host",  &fmt(&data.ssh_bastion_host), sel == 1));
            lines.push(make_row("SSH config file path",  &fmt(&data.ssh_config_path),  sel == 2));
        }

        ConfigureState::DockerK8s { .. } => {
            let fmt = |s: &str| if s.is_empty() { "[not set]".to_string() } else { s.to_string() };
            lines.push(make_row("Default compose file", &fmt(&data.docker_compose_file), sel == 0));
            lines.push(make_row("Default registry URL",  &fmt(&data.docker_registry),    sel == 1));
            lines.push(make_row("Default K8s context",   &fmt(&data.k8s_context),        sel == 2));
            lines.push(make_row("Default K8s namespace", &fmt(&data.k8s_namespace),      sel == 3));
        }

        ConfigureState::Database { .. } => {
            let url_display = if data.db_url.is_empty() {
                "[not set]".to_string()
            } else {
                mask_sensitive(&data.db_url)
            };
            lines.push(make_row("Default connection URL (masked)", &url_display, sel == 0));
            lines.push(make_row(
                "Default schema tool",
                &format!("[{}]  — Enter to cycle (prisma/knex/typeorm)", data.db_schema_tool),
                sel == 1,
            ));
        }

        ConfigureState::MemoryArchive { .. } => {
            lines.push(make_row(
                "Auto-save memory",
                if data.auto_save_memory { "[on]  — Enter to toggle" } else { "[off] — Enter to toggle" },
                sel == 0,
            ));
            lines.push(make_row(
                "Archive frequency (compactions)",
                &data.archive_frequency.to_string(),
                sel == 1,
            ));
            lines.push(make_row(
                "Archive retention (days)",
                &data.archive_retention_days.to_string(),
                sel == 2,
            ));
            lines.push(make_row(
                "Memory directory",
                &(if data.memory_dir.is_empty() { "[default]".to_string() } else { data.memory_dir.clone() }),
                sel == 3,
            ));
        }

        ConfigureState::PluginsCron { .. } => {
            lines.push(make_row(
                "Plugin search paths",
                &(if data.plugin_search_paths.is_empty() { "[default]".to_string() } else { data.plugin_search_paths.clone() }),
                sel == 0,
            ));
            lines.push(make_row(
                "Auto-enable new plugins",
                if data.auto_enable_plugins { "[on]  — Enter to toggle" } else { "[off] — Enter to toggle" },
                sel == 1,
            ));
            lines.push(make_row(
                "Cron scheduler",
                if data.cron_enabled { "[on]  — Enter to toggle" } else { "[off] — Enter to toggle" },
                sel == 2,
            ));
            if data.active_cron_jobs.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "    No active cron jobs.",
                    Style::default().fg(Color::Rgb(0x55, 0x55, 0x66)),
                )));
            } else {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "    Active cron jobs:",
                    Style::default().fg(Color::Rgb(0x88, 0x88, 0x88)),
                )));
                for (i, job) in data.active_cron_jobs.iter().enumerate() {
                    lines.push(make_row(job, "", sel == 3 + i));
                }
            }
        }

        ConfigureState::EditingValue { key, value, cursor, .. } => {
            let prompt = format!("Edit {key}:");
            lines.push(Line::from(Span::styled(
                format!("    {prompt}"),
                Style::default().fg(Color::Yellow),
            )));
            lines.push(Line::from(""));
            let before: String = value.char_indices()
                .take_while(|(i, _)| *i < *cursor)
                .map(|(_, c)| c)
                .collect();
            let cursor_char = value[*cursor..].chars().next().map_or_else(|| " ".to_string(), |c| c.to_string());
            let after = if *cursor < value.len() {
                let next = next_char_boundary(value, *cursor);
                value[next..].to_string()
            } else {
                String::new()
            };
            lines.push(Line::from(vec![
                Span::raw("    ❯ "),
                Span::styled(before, Style::default().fg(Color::White)),
                Span::styled(
                    cursor_char,
                    Style::default().fg(Color::Rgb(0x1a, 0x1a, 0x1a)).bg(Color::White),
                ),
                Span::styled(after, Style::default().fg(Color::White)),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "    Enter to confirm  Esc to cancel",
                Style::default().fg(Color::Rgb(0x44, 0x44, 0x55)),
            )));
        }
    }

    lines.push(Line::from(""));
    lines
}

// ─── Git helpers ──────────────────────────────────────────────────────────────

/// Fetch git branch and diff stats for the current working directory.
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

// ─── Model context helpers ────────────────────────────────────────────────────

/// Return the approximate context window size (in tokens) for a known model.
fn context_max_for_model(model: &str) -> u32 {
    if let Ok(val) = std::env::var("ANVIL_CONTEXT_SIZE") {
        if let Ok(n) = val.replace(['k', 'K'], "000")
            .replace(['m', 'M'], "000000")
            .parse::<u32>() {
            return n;
        }
    }

    let m = model.to_lowercase();
    if m.contains("opus") || m.contains("sonnet") {
        1_000_000
    } else if m.contains("haiku") {
        200_000
    } else if m.starts_with("gpt-4o") {
        128_000
    } else if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
        200_000
    } else if m.contains(':') {
        query_ollama_context_size(model).unwrap_or(32_768)
    } else {
        1_000_000
    }
}

/// Query Ollama's /api/show endpoint for the model's actual context window size.
fn query_ollama_context_size(model: &str) -> Option<u32> {
    let ollama_url = std::env::var("OLLAMA_HOST")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    let output = std::process::Command::new("curl")
        .args(["-s", "--max-time", "2", "-X", "POST",
               "-H", "Content-Type: application/json",
               "-d", &format!("{{\"name\":\"{model}\"}}"),
               &format!("{}/api/show", ollama_url.trim_end_matches('/'))])
        .output()
        .ok()?;
    if !output.status.success() { return None; }
    let val: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    val.pointer("/model_info/context_length")
        .or_else(|| val.pointer("/model_info/num_ctx"))
        .and_then(serde_json::Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
        .or_else(|| {
            val.get("parameters")
                .and_then(|p| p.as_str())
                .and_then(|params| {
                    for line in params.lines() {
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        if parts.len() == 2 && parts[0] == "num_ctx" {
                            return parts[1].parse::<u32>().ok();
                        }
                    }
                    None
                })
        })
}

// init_ollama_model_cache and check_clipboard_for_image are re-exported
// from widgets (see pub use widgets::{...} at the top of this file).
