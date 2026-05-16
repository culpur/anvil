/// Layout B/E — Three-Pane renderer.
///
/// Three horizontal bands:
///   Top third:    FOCUS — current assistant action (pending_text + last streaming log entry).
///   Middle third: LOG   — compact bullets of each log entry (one line per entry).
///   Bottom third: CONTEXT — model, tokens, cost, memory indicators, git, files in scope.
///
/// Vim modal input:
///   `i`   → Insert mode: cursor in input row at bottom of FOCUS pane.
///   `Esc` → Insert → Normal: DISCARDS the draft (locked decision §11 #4).
///   `:`   → Command mode: ex-command line at bottom.
///   `:q`  → exit, `:w` → save draft to Tab.input (no submit).
///   Enter → submit (Insert mode only).
///
/// Layout E (tabs: true): buffer line at row 0 above FOCUS.
/// Layout B (tabs: false): FOCUS starts at row 0.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout as RLayout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;

use super::common::{rgb, render_completion_popup, render_tab_bar};
use super::{LayoutLocalState, TuiLayoutRenderer, VimMode};
use crate::tui::helpers::strip_ansi;
use crate::tui::snapshot::LayoutSnapshot;
use crate::tui::state::LogEntry;
use crate::tui::TabHit;

pub(super) struct Renderer {
    pub tabs: bool,
}

impl TuiLayoutRenderer for Renderer {
    fn render(
        &self,
        frame: &mut Frame,
        snap: &LayoutSnapshot,
        local: &mut LayoutLocalState,
        tab_hits_out: &mut Vec<TabHit>,
    ) {
        let size = frame.area();

        // Extract local state safely.
        let (vim_mode, command_line) = match local {
            LayoutLocalState::ThreePane { vim_mode, command_line } => {
                (vim_mode.clone(), command_line.clone())
            }
            _ => (VimMode::Normal, String::new()),
        };

        // Row 0 (Layout E only): buffer line.
        let mut y_offset: u16 = 0;
        if self.tabs {
            let buffer_line_area = Rect {
                x: size.x,
                y: size.y,
                width: size.width,
                height: 1,
            };
            render_buffer_line(frame, buffer_line_area, snap, tab_hits_out);
            y_offset = 1;
        }

        let remaining = Rect {
            x: size.x,
            y: size.y + y_offset,
            width: size.width,
            height: size.height.saturating_sub(y_offset),
        };

        // Split remaining into three equal horizontal bands.
        let third = remaining.height / 3;
        let focus_h = third.max(3);
        let log_h = third.max(3);
        let context_h = remaining.height.saturating_sub(focus_h + log_h).max(2);

        let bands = RLayout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(focus_h),
                Constraint::Length(log_h),
                Constraint::Min(context_h),
            ])
            .split(remaining);

        let focus_area = bands[0];
        let log_area = bands[1];
        let context_area = bands[2];

        render_focus_pane(frame, focus_area, snap, &vim_mode, &command_line);
        render_log_pane(frame, log_area, snap);
        render_context_pane(frame, context_area, snap);

        // Completion popup above the focus pane input row.
        render_completion_popup(frame, focus_area, snap);

        // Position terminal cursor in Insert mode.
        if vim_mode == VimMode::Insert {
            // Cursor sits on the last row of the FOCUS pane (input row).
            let input_row = focus_area.y + focus_area.height.saturating_sub(1);
            let col = snap.cursor_pos.min((focus_area.width as usize).saturating_sub(3));
            let max_x = focus_area.x + focus_area.width.saturating_sub(1);
            frame.set_cursor_position(Position {
                x: (focus_area.x + 2 + col as u16).min(max_x),
                y: input_row,
            });
        } else if vim_mode == VimMode::Command {
            // Cursor on the ex-command line at the bottom of the context pane.
            let cmd_row = context_area.y + context_area.height.saturating_sub(1);
            let col = command_line.len().min(context_area.width.saturating_sub(2) as usize);
            frame.set_cursor_position(Position {
                x: context_area.x + 1 + col as u16,
                y: cmd_row,
            });
        }
    }
}

// ─── FOCUS pane ───────────────────────────────────────────────────────────────

fn render_focus_pane(
    frame: &mut Frame,
    area: Rect,
    snap: &LayoutSnapshot,
    vim_mode: &VimMode,
    command_line: &str,
) {
    let theme = &snap.theme;
    let width = area.width as usize;

    // Header line: "FOCUS  [NORMAL|INSERT|COMMAND]"
    let mode_label = match vim_mode {
        VimMode::Normal => "NORMAL",
        VimMode::Insert => "INSERT",
        VimMode::Command => "COMMAND",
    };
    let mode_color = match vim_mode {
        VimMode::Normal => Color::DarkGray,
        VimMode::Insert => rgb(theme.accent),
        VimMode::Command => rgb(theme.warning),
    };
    let header = format!("─── FOCUS  [{mode_label}]{}", "─".repeat(width.saturating_sub(18 + mode_label.len())));
    let header_line = Line::from(Span::styled(header, Style::default().fg(mode_color)));

    // Content: pending_text (streaming) or last log entry if no pending.
    let content_lines: Vec<Line<'static>> = if !snap.pending.is_empty() {
        let clean = strip_ansi(&snap.pending);
        clean.lines().map(|l| Line::from(Span::raw(l.to_string()))).collect()
    } else if let Some(last) = snap.log_snapshot.last() {
        match last {
            LogEntry::Assistant(text) => {
                let clean = strip_ansi(text);
                clean.lines().map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(Color::White)))).collect()
            }
            _ => vec![Line::from(Span::styled("Waiting…", Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)))],
        }
    } else {
        vec![Line::from(Span::styled("No conversation yet. Press i to start.", Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)))]
    };

    // Reserve last row for input (Insert mode) or hint (Normal).
    let content_height = area.height.saturating_sub(2) as usize;
    let visible: Vec<Line<'static>> = content_lines
        .into_iter()
        .take(content_height)
        .collect();

    // Input row at bottom.
    let input_row = if *vim_mode == VimMode::Insert {
        Line::from(vec![
            Span::styled("❯ ", Style::default().fg(rgb(theme.accent)).add_modifier(Modifier::BOLD)),
            Span::raw(snap.input_text.clone()),
            Span::styled("█", Style::default().fg(Color::White)),
        ])
    } else if *vim_mode == VimMode::Command {
        Line::from(Span::raw(format!(":{command_line}")))
    } else {
        Line::from(Span::styled(
            "  i to insert  j/k scroll  gt/gT tabs  Ctrl+R deck",
            Style::default().fg(Color::DarkGray),
        ))
    };

    let mut all_lines: Vec<Line<'static>> = vec![header_line];
    all_lines.extend(visible);
    // Pad to content_height rows.
    while all_lines.len() < area.height.saturating_sub(1) as usize {
        all_lines.push(Line::from(""));
    }
    all_lines.push(input_row);

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Paragraph::new(Text::from(all_lines))
            .style(Style::default().bg(rgb(theme.bg_primary)))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        area,
    );
}

// ─── LOG pane ─────────────────────────────────────────────────────────────────

fn render_log_pane(frame: &mut Frame, area: Rect, snap: &LayoutSnapshot) {
    let theme = &snap.theme;
    let width = area.width as usize;

    let header = format!("─── LOG{}", "─".repeat(width.saturating_sub(6)));
    let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled(
        header,
        Style::default().fg(rgb(theme.border)),
    ))];

    let visible_rows = area.height.saturating_sub(1) as usize;
    let log_len = snap.log_snapshot.len();
    let start = log_len.saturating_sub(visible_rows);
    for entry in snap.log_snapshot.iter().skip(start) {
        match entry {
            LogEntry::ToolCall { name, full_result, .. } => {
                let s = if let Some(r) = full_result {
                    let snippet: String = r.chars().take(60).collect();
                    format!("• {name}  {snippet}")
                } else {
                    format!("• {name}  …")
                };
                lines.push(Line::from(Span::styled(
                    s.chars().take(width).collect::<String>(),
                    Style::default().fg(Color::DarkGray),
                )));
                continue;
            }
            LogEntry::Image { .. } => continue,
            _ => {}
        }
        let (prefix, content, color) = match entry {
            LogEntry::User(t) => ("  you  ", t.as_str(), rgb(theme.accent)),
            LogEntry::Assistant(t) => ("  ast  ", t.as_str(), Color::White),
            LogEntry::System(t) => ("  sys  ", t.as_str(), Color::DarkGray),
            _ => unreachable!(),
        };
        let summary: String = content.lines().next().unwrap_or("").chars().take(width.saturating_sub(prefix.len())).collect();
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), Style::default().fg(Color::DarkGray)),
            Span::styled(summary, Style::default().fg(color)),
        ]));
    }

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::default().bg(rgb(theme.bg_primary))),
        area,
    );
}

// ─── CONTEXT pane ─────────────────────────────────────────────────────────────

fn render_context_pane(frame: &mut Frame, area: Rect, snap: &LayoutSnapshot) {
    let theme = &snap.theme;
    let width = area.width as usize;

    let header = format!("─── CONTEXT{}", "─".repeat(width.saturating_sub(10)));
    let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled(
        header,
        Style::default().fg(rgb(theme.border)),
    ))];

    // Model + tokens + cost.
    let cost_str = if snap.model.contains(':') && !snap.model.contains(":cloud") {
        "local".to_string()
    } else {
        format!("in:{} out:{}", snap.input_tokens, snap.output_tokens)
    };
    lines.push(Line::from(vec![
        Span::styled("  Model: ", Style::default().fg(Color::DarkGray)),
        Span::styled(snap.model.clone(), Style::default().fg(Color::Yellow)),
        Span::styled("   ", Style::default()),
        Span::styled(cost_str, Style::default().fg(Color::DarkGray)),
    ]));

    // Git branch + diff.
    if !snap.git_branch.is_empty() {
        let diff = if snap.git_diff_stats.is_empty() {
            "clean".to_string()
        } else {
            snap.git_diff_stats.clone()
        };
        lines.push(Line::from(vec![
            Span::styled("  Git:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(snap.git_branch.clone(), Style::default().fg(Color::Green)),
            Span::styled(format!("  {diff}"), Style::default().fg(Color::DarkGray)),
        ]));
    }

    // Context window.
    if snap.context_max_tokens > 0 {
        let pct = (f64::from(snap.input_tokens) / f64::from(snap.context_max_tokens) * 100.0)
            .min(100.0);
        let bar_w = 16usize;
        let filled = ((pct / 100.0) * bar_w as f64).round() as usize;
        let empty = bar_w.saturating_sub(filled);
        let bar_color = if pct >= 80.0 { Color::Yellow } else { Color::Blue };
        lines.push(Line::from(vec![
            Span::styled("  CTX:   [", Style::default().fg(Color::DarkGray)),
            Span::styled("█".repeat(filled), Style::default().fg(bar_color)),
            Span::styled("░".repeat(empty), Style::default().fg(Color::Rgb(0x33, 0x33, 0x33))),
            Span::styled(
                format!("] {pct:.0}%"),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::default().bg(rgb(theme.bg_primary))),
        area,
    );
}

// ─── Buffer line (Layout E tabs) ──────────────────────────────────────────────

fn render_buffer_line(
    frame: &mut Frame,
    area: Rect,
    snap: &LayoutSnapshot,
    tab_hits_out: &mut Vec<TabHit>,
) {
    // Delegate to the common tab-bar renderer — same visual for all layouts.
    render_tab_bar(frame, area, snap, tab_hits_out);
}
