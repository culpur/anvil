/// Layout A/D — Vertical Split renderer.
///
/// Layout D (`tabs: true`):  tab bar row 0, model bar row 1, content fills
/// the middle, agent panel (optional), footer with input + status lines.
/// Layout A (`tabs: false`): same but the two-row header is collapsed to the
/// model bar only (one row). The tab strip is suppressed; multi-tab state is
/// preserved internally.
///
/// Golden-snapshot contract: with `tabs: true` this renderer MUST produce the
/// exact same bytes as the pre-refactor `AnvilTui::draw()`. The insta snapshots
/// in `tests/snapshots/layout_snapshots__current_tui__*.snap` are the
/// regression net. Run `cargo test --test layout_snapshots` after any change.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout as RLayout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;

use runtime::{format_usd, pricing_for_model};

use super::common::{rgb, render_completion_popup, render_model_bar, render_tab_bar};
use super::{LayoutLocalState, TuiLayoutRenderer};
use crate::tui::configure_types::ConfigureState;
use crate::tui::helpers::{permission_mode_display, strip_ansi};
use crate::tui::layout::{
    compute_input_lines, cursor_visual_position, render_status_lines, StatusLineData,
};
use crate::tui::snapshot::LayoutSnapshot;
use crate::tui::state::THINK_FRAMES;
use crate::tui::TabHit;

/// The Layout A/D renderer. Instantiated with the `tabs` flag per-frame.
pub(super) struct Renderer {
    pub tabs: bool,
}

impl TuiLayoutRenderer for Renderer {
    fn render(
        &self,
        frame: &mut Frame,
        snap: &LayoutSnapshot,
        _local: &mut LayoutLocalState,
        tab_hits_out: &mut Vec<TabHit>,
    ) {
        let size = frame.area();
        let width = size.width as usize;

        // BUG-3 fix (Option B): clear the entire frame area before drawing so
        // stale cells from a previous layout do not bleed through.  The `Clear`
        // widget writes blank cells over every position in `size`, which forces
        // ratatui to treat them as "changed" on the very next diff and re-emit
        // them — regardless of what the backing buffer thought was there.
        frame.render_widget(ratatui::widgets::Clear, size);

        // ── Zone layout ─────────────────────────────────────────────────────────
        // Layout D (tabs: true):  header(2) + content + [agent] + footer
        // Layout A (tabs: false): header(1) + content + [agent] + footer
        let header_rows = if self.tabs { 2 } else { 1 };
        let input_line_count = compute_input_lines(&snap.input_text, width);
        let status_line_count = snap.sl_config.line_count();
        let queued_indicator_height: usize = usize::from(snap.queued_count > 0);
        let footer_height: u16 =
            (2 + queued_indicator_height + input_line_count + status_line_count) as u16;

        let agent_panel_height: u16 = if snap.agent_panel_visible && !snap.agent_rows.is_empty() {
            (snap.agent_rows.len().min(6) as u16) + 2
        } else {
            0
        };

        let chunks = if agent_panel_height > 0 {
            RLayout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(header_rows),
                    Constraint::Min(4),
                    Constraint::Length(agent_panel_height),
                    Constraint::Length(footer_height),
                ])
                .split(size)
        } else {
            let base = RLayout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(header_rows),
                    Constraint::Min(4),
                    Constraint::Length(footer_height),
                ])
                .split(size);
            let mut v = base.to_vec();
            v.push(v[2]);
            v.into()
        };

        let header_area = chunks[0];
        let content_area = chunks[1];
        let (agent_panel_area, footer_area) = if agent_panel_height > 0 {
            (Some(chunks[2]), chunks[3])
        } else {
            (None, chunks[2])
        };

        // ── Header ───────────────────────────────────────────────────────────────
        if self.tabs {
            // Split header into tab bar (row 0) + model bar (row 1).
            let header_split = RLayout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Length(1)])
                .split(header_area);
            let tab_bar_area = header_split[0];
            let model_bar_area = header_split[1];

            render_tab_bar(frame, tab_bar_area, snap, tab_hits_out);
            render_model_bar(frame, model_bar_area, snap);
        } else {
            // Single-row header: model bar only.
            render_model_bar(frame, header_area, snap);
        }

        // ── Content ──────────────────────────────────────────────────────────────
        let configure_state = &snap.configure_state;
        let theme = &snap.theme;
        let content_width = content_area.width;

        // T5-Ssh-D: SSH tabs render the vt100 grid instead of the chat log.
        if snap.is_ssh_tab {
            if let Some((ref grid_lines, ref footer_lines)) = snap.ssh_screen {
                frame.render_widget(ratatui::widgets::Clear, content_area);
                let ssh_footer_height = footer_lines.len() as u16;
                let grid_height = content_area.height.saturating_sub(ssh_footer_height);
                let grid_area = Rect {
                    x: content_area.x,
                    y: content_area.y,
                    width: content_area.width,
                    height: grid_height,
                };
                let status_area = Rect {
                    x: content_area.x,
                    y: content_area.y + grid_height,
                    width: content_area.width,
                    height: ssh_footer_height,
                };
                frame.render_widget(
                    Paragraph::new(Text::from(grid_lines.clone())),
                    grid_area,
                );
                frame.render_widget(
                    Paragraph::new(Text::from(footer_lines.clone())),
                    status_area,
                );
            }
        } else {
            let all_lines: Vec<Line<'static>> = if *configure_state == ConfigureState::Inactive {
                let mut lines: Vec<Line<'static>> = Vec::new();
                for entry in &snap.log_snapshot {
                    lines.extend(
                        entry.to_lines_with(content_width, theme, snap.transcript_verbose),
                    );
                }
                // Streaming assistant text.
                if !snap.pending.is_empty() {
                    let clean = strip_ansi(&snap.pending);
                    lines.extend(clean.lines().map(|l| Line::from(Span::raw(l.to_string()))));
                }
                // Thinking spinner with elapsed-time color warm (#558, CC-141-F).
                if !snap.think.is_empty() {
                    let elapsed_secs = snap.think_elapsed_secs;
                    let elapsed_think = format!("{elapsed_secs:.1}s");
                    let spinner_color = spinner_elapsed_color(
                        elapsed_secs,
                        snap.spinner_warn_secs,
                        snap.spinner_error_secs,
                        rgb(theme.thinking),
                    );
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("{} ", snap.think_frame),
                            Style::default().fg(spinner_color),
                        ),
                        Span::styled(
                            snap.think.clone(),
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
                super::super::render_configure_menu(configure_state, &snap.configure_data, content_width as usize)
            };

            let total_lines = all_lines.len();
            let visible_height = content_area.height as usize;
            let effective_scroll = if *configure_state == ConfigureState::Inactive {
                let max_scroll = total_lines.saturating_sub(visible_height);
                snap.scroll.min(max_scroll)
            } else {
                snap.configure_viewport.offset(total_lines, visible_height)
            };

            // Historical scrollback view.
            let visible_lines: Vec<Line<'static>> =
                if let Some(ref hist_lines) = snap.scrollback_view_lines {
                    let banner_pad = "─".repeat(width.saturating_sub(50));
                    let banner_text = format!(
                        "─── HISTORICAL VIEW  (Press End to return to live) {banner_pad}"
                    );
                    let banner = Line::from(Span::styled(
                        banner_text,
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ));
                    let content_height = visible_height.saturating_sub(1);
                    let mut lines: Vec<Line<'static>> = vec![banner];
                    lines.extend(
                        hist_lines
                            .iter()
                            .take(content_height)
                            .map(|s| Line::from(Span::raw(s.clone()))),
                    );
                    while lines.len() < visible_height {
                        lines.push(Line::from(""));
                    }
                    lines
                } else {
                    all_lines
                        .into_iter()
                        .skip(effective_scroll)
                        .take(visible_height)
                        .collect()
                };

            frame.render_widget(ratatui::widgets::Clear, content_area);
            let content_widget = Paragraph::new(Text::from(visible_lines))
                .style(Style::default().fg(Color::White))
                .wrap(ratatui::widgets::Wrap { trim: false });
            frame.render_widget(content_widget, content_area);
        }

        // ── Agent panel ──────────────────────────────────────────────────────────
        if let Some(panel_area) = agent_panel_area {
            render_agent_panel(frame, panel_area, snap);
        }

        // ── Footer ───────────────────────────────────────────────────────────────
        render_footer(
            frame,
            footer_area,
            snap,
            width,
            queued_indicator_height,
            input_line_count,
        );

        // ── Completion popup ─────────────────────────────────────────────────────
        render_completion_popup(frame, footer_area, snap);
    }
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

fn render_agent_panel(frame: &mut Frame, panel_area: Rect, snap: &LayoutSnapshot) {
    let theme = &snap.theme;
    let panel_width = panel_area.width as usize;
    frame.render_widget(ratatui::widgets::Clear, panel_area);

    let running = snap.agent_rows.iter().filter(|r| r.4 == "⟳").count();
    let done = snap.agent_rows.iter().filter(|r| r.4 == "✓").count();
    let failed = snap.agent_rows.iter().filter(|r| r.4 == "✗").count();
    let mut status_parts = Vec::new();
    if running > 0 { status_parts.push(format!("{running} running")); }
    if done > 0 { status_parts.push(format!("{done} completed")); }
    if failed > 0 { status_parts.push(format!("{failed} failed")); }
    let status_str = status_parts.join(", ");
    let header_label = format!(" Agents ({status_str}) ");
    let dashes_after = "─".repeat(panel_width.saturating_sub(header_label.len() + 2));
    let header_line = Line::from(vec![
        Span::styled("─", Style::default().fg(rgb(theme.border))),
        Span::styled(
            header_label,
            Style::default().fg(rgb(theme.accent)).add_modifier(Modifier::BOLD),
        ),
        Span::styled(dashes_after, Style::default().fg(rgb(theme.border))),
    ]);

    let mut panel_lines: Vec<Line<'static>> = vec![header_line];
    for (id, type_label, task, elapsed, icon) in snap.agent_rows.iter().take(6) {
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
            Span::styled(format!("{id_str}  "), Style::default().fg(Color::DarkGray)),
            Span::styled(type_str, Style::default().fg(rgb(theme.text_secondary))),
            Span::styled(format!("  {task_truncated}"), Style::default().fg(rgb(theme.text_primary))),
            Span::styled(format!("  {elapsed_str} "), Style::default().fg(Color::DarkGray)),
        ]));
    }
    panel_lines.push(Line::from(Span::styled(
        "─".repeat(panel_width),
        Style::default().fg(rgb(theme.border)),
    )));
    frame.render_widget(
        Paragraph::new(Text::from(panel_lines)).style(Style::default().bg(rgb(theme.bg_primary))),
        panel_area,
    );
}

fn render_footer(
    frame: &mut Frame,
    footer_area: Rect,
    snap: &LayoutSnapshot,
    width: usize,
    queued_indicator_height: usize,
    _input_line_count: usize,
) {
    let theme = &snap.theme;
    let configure_state = &snap.configure_state;

    // Separator line.
    let separator = "─".repeat(width);
    let line0 = Line::from(Span::styled(separator, Style::default().fg(rgb(theme.border))));

    // Input area.
    let input_lines_rendered: Vec<Line<'static>> =
        if *configure_state == ConfigureState::Inactive {
            render_input_lines(snap, width)
        } else {
            let breadcrumb = crate::tui::configure_types::configure_breadcrumb(configure_state);
            vec![Line::from(vec![
                Span::styled(
                    "⚒ ",
                    Style::default().fg(rgb(theme.accent)).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    breadcrumb,
                    Style::default().fg(rgb(theme.accent)).add_modifier(Modifier::DIM),
                ),
                Span::styled(
                    "   ↑↓ Navigate  Enter Select  Esc Back",
                    Style::default().fg(rgb(theme.border)),
                ),
            ])]
        };

    let line_blank = Line::from("");

    // Status lines.
    let cost_usd = compute_cost_usd(&snap.model, snap.input_tokens, snap.output_tokens);
    let sl_data = build_sl_data(snap, cost_usd);
    let status_lines = render_status_lines(&snap.sl_config, &sl_data, width);

    // Queued indicator.
    let queued_indicator: Option<Line<'static>> = if snap.queued_count > 0 {
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::styled(
            format!("[{} queued]", snap.queued_count),
            Style::default().fg(rgb(theme.warning)).add_modifier(Modifier::BOLD),
        ));
        for preview in &snap.queued_preview {
            spans.push(Span::styled(
                format!(" • {preview}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if snap.queued_count > snap.queued_preview.len() {
            spans.push(Span::styled(
                format!(" • +{} more", snap.queued_count - snap.queued_preview.len()),
                Style::default().fg(Color::DarkGray),
            ));
        }
        Some(Line::from(spans))
    } else {
        None
    };

    // Assemble footer.
    let mut footer_lines: Vec<Line<'static>> = Vec::new();
    footer_lines.push(line0);
    if let Some(indicator) = queued_indicator {
        footer_lines.push(indicator);
    }
    footer_lines.extend(input_lines_rendered.clone());
    footer_lines.push(line_blank);
    footer_lines.extend(status_lines);
    frame.render_widget(Paragraph::new(Text::from(footer_lines)), footer_area);

    // Cursor position.
    let (cursor_row_offset, cursor_col) =
        cursor_visual_position(&snap.input_text, snap.cursor_pos, width);
    let cursor_x = footer_area.x + cursor_col as u16;
    let cursor_y = footer_area.y + 1 + queued_indicator_height as u16 + cursor_row_offset as u16;
    let max_x = footer_area.x + footer_area.width.saturating_sub(1);
    frame.set_cursor_position(Position {
        x: cursor_x.min(max_x),
        y: cursor_y,
    });
}

/// Build the multi-line input widget lines with inline block cursor.
fn render_input_lines(snap: &LayoutSnapshot, width: usize) -> Vec<Line<'static>> {
    let theme = &snap.theme;
    let prompt_style = Style::default()
        .fg(rgb(theme.accent))
        .add_modifier(Modifier::BOLD);
    let text_style = Style::default().fg(Color::White);
    let cursor_fg = Color::Rgb(0x1a, 0x1a, 0x1a);
    let cursor_bg = Color::White;

    let prompt_width: usize = 2;
    let first_col = width.saturating_sub(prompt_width).max(1);
    let rest_col = width.max(1);

    let mut visual_rows: Vec<Vec<(usize, char)>> = Vec::new();
    let mut current_row_chars: Vec<(usize, char)> = Vec::new();
    let mut byte_off: usize = 0;
    let logical_segs: Vec<&str> = snap.input_text.split('\n').collect();
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
    visual_rows.push(current_row_chars);
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
            if !cursor_placed && boff == snap.cursor_pos {
                cur_str.push(ch);
                cursor_placed = true;
            } else if boff < snap.cursor_pos {
                before.push(ch);
            } else {
                after.push(ch);
            }
        }

        let trailing_cursor = !cursor_placed && is_last_row && snap.cursor_pos >= snap.input_text.len();

        let mut spans: Vec<Span<'static>> = Vec::new();
        if row_idx == 0 {
            spans.push(Span::styled("❯ ", prompt_style));
        }
        if !before.is_empty() {
            spans.push(Span::styled(before, text_style));
        }
        if cursor_placed {
            spans.push(Span::styled(cur_str, Style::default().fg(cursor_fg).bg(cursor_bg)));
            if !after.is_empty() {
                spans.push(Span::styled(after, text_style));
            }
        } else if trailing_cursor {
            spans.push(Span::styled(" ", Style::default().fg(cursor_fg).bg(cursor_bg)));
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
}

fn compute_cost_usd(model: &str, input_tokens: u32, output_tokens: u32) -> String {
    if model.contains(':') && !model.contains(":cloud") {
        "local".to_string()
    } else if let Some(p) = pricing_for_model(model) {
        let cost = (f64::from(input_tokens) / 1_000_000.0) * p.input_cost_per_million
            + (f64::from(output_tokens) / 1_000_000.0) * p.output_cost_per_million;
        format_usd(cost)
    } else if model.contains(':') {
        "cloud".to_string()
    } else {
        format_usd(0.0)
    }
}

fn build_sl_data(snap: &LayoutSnapshot, cost_usd: String) -> StatusLineData {
    StatusLineData {
        model: snap.model.clone(),
        thinking_enabled: snap.thinking_enabled,
        input_tokens: snap.input_tokens,
        output_tokens: snap.output_tokens,
        cost_usd,
        context_used: snap.input_tokens,
        context_max: snap.context_max_tokens,
        elapsed_secs: snap.elapsed.as_secs(),
        git_branch: snap.git_branch.clone(),
        git_diff: snap.git_diff_stats.clone(),
        git_clean: snap.git_diff_stats.is_empty(),
        permission_mode: permission_mode_display(&snap.permission_mode),
        qmd_status: snap.qmd_status.clone(),
        archive_status: snap.last_archive_status.clone(),
        update_available: snap.update_available.clone(),
        remote_url: snap.remote_url.clone(),
        remote_code: snap.remote_code.clone(),
        vim_mode: false,
        version: env!("CARGO_PKG_VERSION").to_string(),
        provider: String::new(),
        token_speed: 0.0,
        burn_rate_hr: 0.0,
        cost_daily: 0.0,
        cost_weekly: 0.0,
        cost_monthly: 0.0,
        cache_hit_pct: 0.0,
        lines_added: snap.lines_added,
        lines_removed: snap.lines_removed,
        mcp_server_count: {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .and_then(|h| {
                    std::fs::read_to_string(h.join(".anvil").join("settings.json")).ok()
                })
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| {
                    v.get("mcpServers")
                        .and_then(|m| m.as_object())
                        .map(|o| o.len() as u32)
                })
                .unwrap_or(0)
        },
        effort_level: snap.effort_level.clone(),
        accent: snap.theme.accent,
        warning: snap.theme.warning,
        success: snap.theme.success,
        error: snap.theme.error,
    }
}

// Re-export the think frame lookup so it's available for the unused import warning suppression.
#[allow(dead_code)]
fn _use_think_frames(idx: usize) -> &'static str {
    THINK_FRAMES[idx % THINK_FRAMES.len()]
}

/// Return the spinner foreground color based on elapsed thinking seconds.
///
/// - 0 .. warn_secs      → `default_color` (typically theme.thinking = green)
/// - warn_secs .. error_secs → amber (yellow)
/// - error_secs+         → red
///
/// Thresholds are read at startup from `ANVIL_SPINNER_WARN_SECS` (default 10)
/// and `ANVIL_SPINNER_ERROR_SECS` (default 30). Both are stored in
/// `AnvilTui.spinner_warn_secs` / `spinner_error_secs` and forwarded through
/// the `LayoutSnapshot` so the renderer is a pure function of its inputs.
pub(crate) fn spinner_elapsed_color(
    elapsed_secs: f64,
    warn_secs: u64,
    error_secs: u64,
    default_color: Color,
) -> Color {
    let secs = elapsed_secs as u64;
    if secs >= error_secs {
        Color::Red
    } else if secs >= warn_secs {
        Color::Yellow
    } else {
        default_color
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;
    use super::spinner_elapsed_color;

    const GREEN: Color = Color::Green;

    #[test]
    fn spinner_color_green_under_warn_threshold() {
        // 9 seconds, warn=10, error=30 → still green
        assert_eq!(spinner_elapsed_color(9.0, 10, 30, GREEN), GREEN);
    }

    #[test]
    fn spinner_color_amber_between_warn_and_error() {
        // 15 seconds, warn=10, error=30 → amber (yellow)
        assert_eq!(spinner_elapsed_color(15.0, 10, 30, GREEN), Color::Yellow);
    }

    #[test]
    fn spinner_color_red_above_error() {
        // 30+ seconds → red
        assert_eq!(spinner_elapsed_color(30.0, 10, 30, GREEN), Color::Red);
        assert_eq!(spinner_elapsed_color(60.0, 10, 30, GREEN), Color::Red);
    }

    #[test]
    fn spinner_color_respects_env_override() {
        // Custom thresholds: warn=5, error=15
        assert_eq!(spinner_elapsed_color(4.9, 5, 15, GREEN), GREEN);
        assert_eq!(spinner_elapsed_color(5.0, 5, 15, GREEN), Color::Yellow);
        assert_eq!(spinner_elapsed_color(15.0, 5, 15, GREEN), Color::Red);
    }
}
