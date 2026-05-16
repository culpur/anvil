/// Layout A/D — Vertical Split rail+deck renderer.
///
/// Visual design:
/// ```text
/// ┌─[Anvil]────────────────────────────────────────────────────────────────────┐
/// │ ⊢ sessions      │                                                          │
/// │ ● anvil v2.2.16 │   DECK: conversation                                     │
/// │ ○ aegis-fix     │                                                          │
/// │ ○ deploy-prep   │   you  ▌ refactor the auth module…                       │
/// │                 │                                                          │
/// │ ⊢ agents        │   anvil ▎ reading 4 files. drafting plan…                │
/// │ │ none active   │                                                          │
/// │                 │                                                          │
/// │ ⊢ status        │                                                          │
/// │ • anthropic     │                                                          │
/// │   sonnet-4.6    │                                                          │
/// │ • workspace     │                                                          │
/// │   anvil-dev     │                                                          │
/// │ • cost          │                                                          │
/// │   $0.42         │ ──────────────────────────────────────────────────────── │
/// │                 │ ▌ <prompt>                                                │
/// └─────────────────┴──────────────────────────────────────────────────────────┘
/// Keys: g switch deck │ d tools │ s sessions │ a agents │ ? help
/// ```
///
/// Layout D (`tabs: true`): tab strip at top of right deck (NOT full-width).
/// Layout A (`tabs: false`): no tab strip; left rail anchored.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout as RLayout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;

use runtime::{format_usd, pricing_for_model};

use super::common::{rgb, render_completion_popup, render_tab_bar};
use super::{LayoutLocalState, RightDeckMode, TuiLayoutRenderer};
use crate::tui::configure_types::ConfigureState;
use crate::tui::helpers::{permission_mode_display, strip_ansi};
use crate::tui::layout::{
    compute_input_lines, cursor_visual_position, render_status_lines, StatusLineData,
};
use crate::tui::snapshot::LayoutSnapshot;
use crate::tui::state::LogEntry;
use crate::tui::TabHit;

/// Rail width in columns. Clamped between MIN_RAIL and MAX_RAIL.
const MIN_RAIL: u16 = 16;
const MAX_RAIL: u16 = 32;
const DEFAULT_RAIL: u16 = 24;

/// Compute the left rail width from terminal width.
fn rail_width(terminal_width: u16) -> u16 {
    if terminal_width < MIN_RAIL + 20 {
        // Too narrow — collapse the rail entirely.
        0
    } else {
        DEFAULT_RAIL.clamp(MIN_RAIL, MAX_RAIL.min(terminal_width / 4))
    }
}

/// The Layout A/D rail+deck renderer. Instantiated with the `tabs` flag per-frame.
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
        let width = size.width;

        // BUG-3 fix: wipe all cells before drawing so stale content from a
        // previous layout cannot survive through ratatui's frame diff.
        frame.render_widget(ratatui::widgets::Clear, size);

        // Determine current deck mode from local state.
        let deck_mode = match local {
            LayoutLocalState::VerticalSplit { right_deck_mode, .. } => *right_deck_mode,
            _ => RightDeckMode::Conversation,
        };

        let rail_w = rail_width(width);

        // ── Global vertical split: left rail | right deck ───────────────────────
        // When rail_w == 0 (narrow terminal) we skip the rail entirely.
        if rail_w == 0 {
            // Fallback: render right deck full-width (same as classic layout).
            render_deck(frame, size, snap, local, tab_hits_out, self.tabs, deck_mode);
            return;
        }

        let horiz = RLayout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(rail_w), Constraint::Fill(1)])
            .split(size);
        let rail_area = horiz[0];
        let deck_area = horiz[1];

        render_rail(frame, rail_area, snap);
        render_deck(frame, deck_area, snap, local, tab_hits_out, self.tabs, deck_mode);

        // ── Keybind hint row at very bottom of rail ─────────────────────────────
        // We don't draw a separate row; the keys line is inside render_deck footer.
    }
}

// ─── Rail renderer ────────────────────────────────────────────────────────────

fn render_rail(frame: &mut Frame, area: Rect, snap: &LayoutSnapshot) {
    let theme = &snap.theme;
    let w = area.width as usize;

    frame.render_widget(ratatui::widgets::Clear, area);

    let mut lines: Vec<Line<'static>> = Vec::new();

    // ── Sessions section ──────────────────────────────────────────────────────
    let section_style = Style::default().fg(rgb(theme.accent)).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    let active_dot_style = Style::default().fg(rgb(theme.accent)).add_modifier(Modifier::BOLD);
    let inactive_dot_style = Style::default().fg(Color::DarkGray);

    lines.push(Line::from(Span::styled(
        truncate(format!(" ⊢ sessions{}", " ".repeat(w.saturating_sub(11))), w),
        section_style,
    )));

    for (_, tab_name, is_active, has_unread, _has_perm) in &snap.tab_infos {
        let dot = if *is_active { "●" } else { "○" };
        let dot_style = if *is_active { active_dot_style } else { inactive_dot_style };
        let name_style = if *is_active {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Rgb(0xaa, 0xaa, 0xaa))
        };
        let unread_marker = if *has_unread { "·" } else { " " };
        let available = w.saturating_sub(4);
        let name_truncated = truncate(tab_name.clone(), available);
        lines.push(Line::from(vec![
            Span::styled(format!(" {dot} "), dot_style),
            Span::styled(name_truncated, name_style),
            Span::styled(unread_marker.to_string(), dim),
        ]));
    }
    lines.push(Line::from(""));

    // ── Agents section ────────────────────────────────────────────────────────
    lines.push(Line::from(Span::styled(
        truncate(format!(" ⊢ agents{}", " ".repeat(w.saturating_sub(9))), w),
        section_style,
    )));

    if snap.agent_rows.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" │ ", dim),
            Span::styled("none active".to_string(), dim),
        ]));
    } else {
        for (_, type_label, _task, elapsed, icon) in snap.agent_rows.iter().take(4) {
            let icon_style = match *icon {
                "⟳" => Style::default().fg(rgb(theme.accent)),
                "✓" => Style::default().fg(rgb(theme.success)),
                "✗" => Style::default().fg(rgb(theme.error)),
                _ => dim,
            };
            let label = truncate(format!("{icon} {type_label} {elapsed}"), w.saturating_sub(2));
            lines.push(Line::from(vec![
                Span::styled(" ", Style::default()),
                Span::styled(label, icon_style),
            ]));
        }
    }
    lines.push(Line::from(""));

    // ── Status section ────────────────────────────────────────────────────────
    lines.push(Line::from(Span::styled(
        truncate(format!(" ⊢ status{}", " ".repeat(w.saturating_sub(9))), w),
        section_style,
    )));

    // Provider + model
    let model_short = snap.model.split('/').last().unwrap_or(&snap.model);
    let model_display = if model_short.len() > w.saturating_sub(4) {
        truncate(model_short.to_string(), w.saturating_sub(4))
    } else {
        model_short.to_string()
    };
    // Provider comes from model prefix (e.g. "claude-…" → anthropic, "gpt-…" → openai)
    let provider = if snap.model.starts_with("claude") || snap.model.contains("anthropic") {
        "anthropic"
    } else if snap.model.starts_with("gpt") || snap.model.starts_with("o1") || snap.model.starts_with("o3") {
        "openai"
    } else if snap.model.contains("qwen") || snap.model.contains("deepseek") {
        "local"
    } else {
        "provider"
    };
    lines.push(Line::from(vec![
        Span::styled(" • ", dim),
        Span::styled(truncate(provider.to_string(), w.saturating_sub(3)), Style::default().fg(Color::Rgb(0xaa, 0xaa, 0xaa))),
    ]));
    lines.push(Line::from(vec![
        Span::styled("   ", Style::default()),
        Span::styled(model_display, Style::default().fg(Color::Yellow)),
    ]));

    // Workspace (git branch)
    if !snap.git_branch.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" • ", dim),
            Span::styled("workspace", Style::default().fg(Color::Rgb(0xaa, 0xaa, 0xaa))),
        ]));
        lines.push(Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled(
                truncate(snap.git_branch.clone(), w.saturating_sub(3)),
                Style::default().fg(Color::Green),
            ),
        ]));
    }

    // Cost
    let cost_str = if snap.model.contains(':') && !snap.model.contains(":cloud") {
        "local".to_string()
    } else if let Some(p) = pricing_for_model(&snap.model) {
        let cost = (f64::from(snap.input_tokens) / 1_000_000.0) * p.input_cost_per_million
            + (f64::from(snap.output_tokens) / 1_000_000.0) * p.output_cost_per_million;
        format_usd(cost)
    } else {
        format_usd(0.0)
    };
    lines.push(Line::from(vec![
        Span::styled(" • ", dim),
        Span::styled("cost", Style::default().fg(Color::Rgb(0xaa, 0xaa, 0xaa))),
    ]));
    lines.push(Line::from(vec![
        Span::styled("   ", Style::default()),
        Span::styled(cost_str, Style::default().fg(Color::Rgb(0x88, 0xcc, 0x88))),
    ]));

    // Fill remaining rows.
    let total = area.height as usize;
    while lines.len() < total {
        lines.push(Line::from(""));
    }
    lines.truncate(total);

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(rgb(theme.bg_primary))),
        area,
    );
}

// ─── Deck renderer ────────────────────────────────────────────────────────────

fn render_deck(
    frame: &mut Frame,
    area: Rect,
    snap: &LayoutSnapshot,
    local: &mut LayoutLocalState,
    tab_hits_out: &mut Vec<TabHit>,
    tabs: bool,
    deck_mode: RightDeckMode,
) {
    let theme = &snap.theme;
    let width = area.width as usize;

    // How many rows the input area needs.
    let input_line_count = compute_input_lines(&snap.input_text, width);
    let status_line_count = snap.sl_config.line_count();
    let queued_indicator_height: usize = usize::from(snap.queued_count > 0);
    // footer: separator(1) + [queued] + input_lines + blank(1) + status_lines
    let footer_height: u16 =
        (2 + queued_indicator_height + input_line_count + status_line_count) as u16;

    // Keybind row at the very bottom.
    let keybind_height: u16 = 1;

    // Optionally a tab strip at the top of the deck.
    let tab_strip_height: u16 = if tabs { 1 } else { 0 };

    // Deck header (DECK: <mode>) — 1 row.
    let deck_header_height: u16 = 1;

    let constraints = {
        let mut c = vec![
            Constraint::Length(tab_strip_height + deck_header_height),
        ];
        c.push(Constraint::Fill(1)); // content
        c.push(Constraint::Length(footer_height));
        c.push(Constraint::Length(keybind_height));
        c
    };

    let chunks = RLayout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let header_area = chunks[0];
    let content_area = chunks[1];
    let footer_area = chunks[2];
    let keybind_area = chunks[3];

    // ── Header: [optional tab strip] + deck mode label ────────────────────────
    frame.render_widget(ratatui::widgets::Clear, header_area);
    if tabs && tab_strip_height > 0 {
        // Tab strip occupies the first row of the header area.
        let tab_area = Rect {
            x: header_area.x,
            y: header_area.y,
            width: header_area.width,
            height: 1,
        };
        render_tab_bar(frame, tab_area, snap, tab_hits_out);
        // Deck mode header on next row.
        if header_area.height > 1 {
            let mode_area = Rect {
                x: header_area.x,
                y: header_area.y + 1,
                width: header_area.width,
                height: 1,
            };
            render_deck_header(frame, mode_area, snap, deck_mode);
        }
    } else {
        render_deck_header(frame, header_area, snap, deck_mode);
    }

    // ── Content ───────────────────────────────────────────────────────────────
    let configure_state = &snap.configure_state;
    let content_width = content_area.width;
    frame.render_widget(ratatui::widgets::Clear, content_area);

    if snap.is_ssh_tab {
        if let Some((ref grid_lines, ref footer_lines)) = snap.ssh_screen {
            let ssh_footer_h = footer_lines.len() as u16;
            let grid_h = content_area.height.saturating_sub(ssh_footer_h);
            let grid_area = Rect {
                x: content_area.x, y: content_area.y,
                width: content_area.width, height: grid_h,
            };
            let status_area = Rect {
                x: content_area.x, y: content_area.y + grid_h,
                width: content_area.width, height: ssh_footer_h,
            };
            frame.render_widget(Paragraph::new(Text::from(grid_lines.clone())), grid_area);
            frame.render_widget(Paragraph::new(Text::from(footer_lines.clone())), status_area);
        }
    } else {
        let all_lines: Vec<Line<'static>> = if *configure_state == ConfigureState::Inactive {
            build_content_lines(snap, content_width, deck_mode, theme)
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

        let visible_lines: Vec<Line<'static>> =
            if let Some(ref hist_lines) = snap.scrollback_view_lines {
                let banner_pad = "─".repeat(width.saturating_sub(50));
                let banner_text = format!(
                    "─── HISTORICAL VIEW  (Press End to return to live) {banner_pad}"
                );
                let banner = Line::from(Span::styled(
                    banner_text,
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ));
                let content_h = visible_height.saturating_sub(1);
                let mut lines: Vec<Line<'static>> = vec![banner];
                lines.extend(hist_lines.iter().take(content_h).map(|s| Line::from(Span::raw(s.clone()))));
                while lines.len() < visible_height {
                    lines.push(Line::from(""));
                }
                lines
            } else {
                all_lines.into_iter().skip(effective_scroll).take(visible_height).collect()
            };

        frame.render_widget(
            Paragraph::new(Text::from(visible_lines))
                .style(Style::default().fg(Color::White))
                .wrap(ratatui::widgets::Wrap { trim: false }),
            content_area,
        );
    }

    // ── Footer ────────────────────────────────────────────────────────────────
    render_footer(frame, footer_area, snap, width, queued_indicator_height);

    // ── Keybind row ───────────────────────────────────────────────────────────
    frame.render_widget(ratatui::widgets::Clear, keybind_area);
    let keybind_text = "g switch deck │ d tools │ s sessions │ a agents │ ? help";
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate(keybind_text.to_string(), width),
            Style::default().fg(Color::DarkGray),
        )),
        keybind_area,
    );

    // ── Completion popup ──────────────────────────────────────────────────────
    render_completion_popup(frame, footer_area, snap);

    // ── Cursor position ───────────────────────────────────────────────────────
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

fn render_deck_header(frame: &mut Frame, area: Rect, snap: &LayoutSnapshot, deck_mode: RightDeckMode) {
    let theme = &snap.theme;
    let w = area.width as usize;
    let mode_label = deck_mode.label();
    let header_text = format!("DECK: {mode_label}{}", " ".repeat(w.saturating_sub(6 + mode_label.len())));
    frame.render_widget(
        Paragraph::new(Span::styled(
            header_text,
            Style::default().fg(rgb(theme.accent)).add_modifier(Modifier::BOLD | Modifier::DIM),
        )),
        area,
    );
}

fn build_content_lines(
    snap: &LayoutSnapshot,
    content_width: u16,
    deck_mode: RightDeckMode,
    theme: &runtime::Theme,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    match deck_mode {
        RightDeckMode::Conversation => {
            for entry in &snap.log_snapshot {
                lines.extend(entry.to_lines_with(content_width, theme, snap.transcript_verbose));
            }
            // Streaming assistant text.
            if !snap.pending.is_empty() {
                let clean = strip_ansi(&snap.pending);
                lines.extend(clean.lines().map(|l| Line::from(Span::raw(l.to_string()))));
            }
            // Thinking spinner.
            if !snap.think.is_empty() {
                use super::classic::spinner_elapsed_color;
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
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                    ),
                    Span::styled(
                        format!("  ({elapsed_think})"),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }
        RightDeckMode::Transcript => {
            // Full transcript with every entry expanded.
            for entry in &snap.log_snapshot {
                lines.extend(entry.to_lines_with(content_width, theme, true));
            }
        }
        RightDeckMode::ToolResults => {
            // Only tool-call cards.
            for entry in &snap.log_snapshot {
                if matches!(entry, LogEntry::ToolCall { .. }) {
                    lines.extend(entry.to_lines_with(content_width, theme, snap.transcript_verbose));
                }
            }
            if lines.is_empty() {
                lines.push(Line::from(Span::styled(
                    "No tool calls yet.",
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                )));
            }
        }
    }

    lines
}

fn render_footer(
    frame: &mut Frame,
    footer_area: Rect,
    snap: &LayoutSnapshot,
    width: usize,
    queued_indicator_height: usize,
) {
    let theme = &snap.theme;
    let configure_state = &snap.configure_state;

    let separator = "─".repeat(width);
    let line0 = Line::from(Span::styled(separator, Style::default().fg(rgb(theme.border))));

    let input_lines_rendered: Vec<Line<'static>> =
        if *configure_state == ConfigureState::Inactive {
            render_input_lines(snap, width)
        } else {
            let breadcrumb = crate::tui::configure_types::configure_breadcrumb(configure_state);
            vec![Line::from(vec![
                Span::styled("⚒ ", Style::default().fg(rgb(theme.accent)).add_modifier(Modifier::BOLD)),
                Span::styled(breadcrumb, Style::default().fg(rgb(theme.accent)).add_modifier(Modifier::DIM)),
                Span::styled("   ↑↓ Navigate  Enter Select  Esc Back", Style::default().fg(rgb(theme.border))),
            ])]
        };

    let line_blank = Line::from("");

    let cost_usd = compute_cost_usd(&snap.model, snap.input_tokens, snap.output_tokens);
    let sl_data = build_sl_data(snap, cost_usd);
    let status_lines = render_status_lines(&snap.sl_config, &sl_data, width);

    let queued_indicator: Option<Line<'static>> = if snap.queued_count > 0 {
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::styled(
            format!("[{} queued]", snap.queued_count),
            Style::default().fg(rgb(theme.warning)).add_modifier(Modifier::BOLD),
        ));
        for preview in &snap.queued_preview {
            spans.push(Span::styled(format!(" • {preview}"), Style::default().fg(Color::DarkGray)));
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

    let mut footer_lines: Vec<Line<'static>> = Vec::new();
    footer_lines.push(line0);
    if let Some(indicator) = queued_indicator {
        footer_lines.push(indicator);
    }
    footer_lines.extend(input_lines_rendered);
    footer_lines.push(line_blank);
    footer_lines.extend(status_lines);
    frame.render_widget(Paragraph::new(Text::from(footer_lines)), footer_area);
}

/// Build the multi-line input widget lines with inline block cursor.
/// Shared with classic.rs via re-use of the same pattern.
fn render_input_lines(snap: &LayoutSnapshot, width: usize) -> Vec<Line<'static>> {
    use super::classic::render_input_lines as classic_input;
    classic_input(snap, width)
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
    super::classic::build_sl_data(snap, cost_usd)
}

/// Truncate a string to at most `max_chars` display characters.
fn truncate(s: String, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s
    } else {
        let t: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

// ─── Golden snapshot tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn extract_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let (bw, bh) = (buf.area.width as usize, buf.area.height as usize);
        let mut rows = Vec::with_capacity(bh);
        for row in 0..bh {
            let mut line = String::with_capacity(bw);
            for col in 0..bw {
                let ch = buf[(col as u16, row as u16)].symbol();
                if ch.is_empty() || ch == "\x00" {
                    line.push(' ');
                } else {
                    line.push_str(ch);
                }
            }
            rows.push(line.trim_end().to_string());
        }
        rows.join("\n")
    }

    fn make_snap(width: u16, height: u16, tabs: bool) -> String {
        use ratatui::Frame;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("TestBackend");

        terminal.draw(|frame: &mut Frame| {
            let size = frame.area();
            let w = size.width as usize;

            // Minimal fixture snapshot.
            use ratatui::layout::{Constraint, Direction, Layout as RLayout};
            use ratatui::style::{Color, Modifier, Style};
            use ratatui::text::{Line, Span, Text};
            use ratatui::widgets::Paragraph;

            // Horizontal split: rail(24) | deck(rest).
            let rail_w = super::rail_width(size.width);
            if rail_w == 0 {
                frame.render_widget(Paragraph::new("(narrow)"), size);
                return;
            }
            let horiz = RLayout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(rail_w), Constraint::Fill(1)])
                .split(size);
            let rail_area = horiz[0];
            let deck_area = horiz[1];
            let deck_w = deck_area.width as usize;

            // Rail content.
            let rail_lines: Vec<Line<'static>> = vec![
                Line::from(Span::styled(" ⊢ sessions", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                Line::from(Span::styled(" ● main", Style::default().fg(Color::White).add_modifier(Modifier::BOLD))),
                Line::from(""),
                Line::from(Span::styled(" ⊢ agents", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                Line::from(Span::styled(" │ none active", Style::default().fg(Color::DarkGray))),
                Line::from(""),
                Line::from(Span::styled(" ⊢ status", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                Line::from(Span::styled(" • anthropic", Style::default().fg(Color::Rgb(0xaa, 0xaa, 0xaa)))),
                Line::from(Span::styled("   sonnet-4-6", Style::default().fg(Color::Yellow))),
            ];
            frame.render_widget(
                Paragraph::new(Text::from(rail_lines)).style(Style::default()),
                rail_area,
            );

            // Deck content.
            let deck_header = if tabs {
                format!("[1: main]   DECK: Conversation{}", " ".repeat(deck_w.saturating_sub(32)))
            } else {
                format!("DECK: Conversation{}", " ".repeat(deck_w.saturating_sub(18)))
            };
            let sep = "─".repeat(deck_w);
            let deck_lines: Vec<Line<'static>> = vec![
                Line::from(Span::styled(deck_header, Style::default().fg(Color::Cyan))),
                Line::from(""),
                Line::from(Span::raw("You  do the thing")),
                Line::from(""),
                Line::from(Span::raw("Anvil  ok")),
                Line::from(""),
            ];
            let mut all: Vec<Line<'static>> = deck_lines;
            while all.len() < deck_area.height.saturating_sub(3) as usize {
                all.push(Line::from(""));
            }
            all.push(Line::from(Span::raw(sep.clone())));
            all.push(Line::from(Span::raw(format!("> next prompt"))));
            all.push(Line::from(Span::styled(
                "g switch deck │ d tools │ ? help",
                Style::default().fg(Color::DarkGray),
            )));
            frame.render_widget(
                Paragraph::new(Text::from(all)).style(Style::default()),
                deck_area,
            );

            let _ = w; // silence unused warning
        }).expect("terminal.draw");

        extract_text(&terminal)
    }

    #[test]
    fn vertical_split__80x24() {
        let rendered = make_snap(80, 24, false);
        // Verify left rail is present (sessions section visible).
        assert!(rendered.contains("⊢ sessions"), "rail sessions header must be visible");
        assert!(rendered.contains("⊢ agents"), "rail agents header must be visible");
        assert!(rendered.contains("DECK:"), "deck header must be visible");
        insta::assert_snapshot!("vertical_split__80x24", rendered);
    }

    #[test]
    fn vertical_split__120x40() {
        let rendered = make_snap(120, 40, false);
        assert!(rendered.contains("⊢ sessions"), "rail sessions header must be visible");
        insta::assert_snapshot!("vertical_split__120x40", rendered);
    }

    #[test]
    fn vertical_split__200x60() {
        let rendered = make_snap(200, 60, false);
        assert!(rendered.contains("⊢ sessions"), "rail sessions header must be visible");
        insta::assert_snapshot!("vertical_split__200x60", rendered);
    }

    #[test]
    fn vertical_split_tabs__80x24() {
        let rendered = make_snap(80, 24, true);
        assert!(rendered.contains("DECK:"), "deck header must be visible in tabs variant");
        insta::assert_snapshot!("vertical_split_tabs__80x24", rendered);
    }

    #[test]
    fn vertical_split_tabs__120x40() {
        let rendered = make_snap(120, 40, true);
        insta::assert_snapshot!("vertical_split_tabs__120x40", rendered);
    }
}
