/// Layout C/F — Journal renderer.
///
/// Single-column timestamped journal view with Ctrl-K command palette.
///
/// Layout F (tabs: true): thread-switcher anchor line at row 0 with
///   `anvil-dev · aegis-fix · deploy-prep · +`
/// Layout C (tabs: false): anchor line absent, multi-thread state still
///   accessible via Ctrl+K palette "Switch thread: <name>" command.
///
/// Input: always-visible bottom row with `▌ ctrl-k for command palette`.
///
/// Ctrl-K palette (locked: full VS Code scope):
///   Fuzzy search over: slash commands, thread names, recent prompts, files.
///   Ctrl+J/Ctrl+K navigate inside palette (Emacs muscle memory).
///   Enter executes. Esc closes.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout as RLayout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;

use super::common::rgb;
use super::{LayoutLocalState, TuiLayoutRenderer};
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
        let theme = &snap.theme;

        // BUG-3 fix (Option B): clear the entire frame so cells from a prior
        // layout do not survive ratatui's frame-diff into this layout's first frame.
        frame.render_widget(ratatui::widgets::Clear, size);

        // Extract local state.
        let (palette_open, palette_query, palette_selected) = match local {
            LayoutLocalState::Journal {
                palette_open,
                palette_query,
                palette_selected,
            } => (*palette_open, palette_query.clone(), *palette_selected),
            _ => (false, String::new(), 0),
        };

        // ── Row allocation ────────────────────────────────────────────────────
        // Layout F: row 0 = thread switcher; rest = header + body + input row.
        // Layout C: no thread switcher row.
        let mut y_offset: u16 = 0;
        if self.tabs {
            let anchor_area = Rect {
                x: size.x,
                y: size.y,
                width: size.width,
                height: 1,
            };
            render_thread_switcher(frame, anchor_area, snap, tab_hits_out);
            y_offset = 1;
        }

        let remaining = Rect {
            x: size.x,
            y: size.y + y_offset,
            width: size.width,
            height: size.height.saturating_sub(y_offset),
        };

        // remaining = header(1) + body(fill) + input_row(1).
        let bands = RLayout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(remaining);

        let header_area = bands[0];
        let body_area = bands[1];
        let input_area = bands[2];

        // ── Header ────────────────────────────────────────────────────────────
        let version = env!("CARGO_PKG_VERSION");
        let header_text = format!(
            " {} · v{} · {}",
            if snap.git_branch.is_empty() { "anvil".to_string() } else { snap.git_branch.clone() },
            version,
            snap.model,
        );
        frame.render_widget(
            Paragraph::new(header_text).style(
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ),
            header_area,
        );

        // ── Body (timestamped journal entries) ────────────────────────────────
        render_journal_body(frame, body_area, snap);

        // ── Input row ─────────────────────────────────────────────────────────
        let input_line = if snap.input_text.is_empty() {
            Line::from(Span::styled(
                " ▌ ctrl-k for command palette · ↑ history · enter to send",
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            Line::from(vec![
                Span::styled(" ▌ ", Style::default().fg(rgb(theme.accent))),
                Span::raw(snap.input_text.clone()),
                Span::styled("█", Style::default().fg(Color::White)),
            ])
        };
        frame.render_widget(
            Paragraph::new(input_line).style(Style::default().bg(rgb(theme.bg_primary))),
            input_area,
        );

        // Cursor in the input row.
        let col = snap.cursor_pos.min(input_area.width.saturating_sub(4) as usize);
        frame.set_cursor_position(Position {
            x: input_area.x + 3 + col as u16,
            y: input_area.y,
        });

        // ── Ctrl-K palette overlay ─────────────────────────────────────────────
        if palette_open {
            render_palette(frame, size, snap, &palette_query, palette_selected);
        }
    }
}

// ─── Thread switcher (Layout F) ───────────────────────────────────────────────

fn render_thread_switcher(
    frame: &mut Frame,
    area: Rect,
    snap: &LayoutSnapshot,
    tab_hits_out: &mut Vec<TabHit>,
) {
    use crate::tui::TabHit;
    let theme = &snap.theme;
    let width = area.width as usize;

    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    let mut cursor_col: u16 = area.x + 1;

    for (idx, (tab_id, tab_name, is_active, _has_unread, _has_perm)) in
        snap.tab_infos.iter().enumerate()
    {
        let label = tab_name.clone();
        let label_len = label.chars().count() as u16;
        let label_start = cursor_col;
        let label_end = cursor_col + label_len;

        let style = if *is_active {
            Style::default()
                .fg(rgb(theme.accent))
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(rgb(theme.text_secondary))
        };
        spans.push(Span::styled(label, style));
        tab_hits_out.push(TabHit { idx, label_start, label_end, close_col: None });

        // Separator between tabs, " · " or " + " for the new-tab button.
        if idx + 1 < snap.tab_infos.len() {
            spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
            cursor_col = label_end + 3;
        } else {
            spans.push(Span::styled(" · +", Style::default().fg(Color::DarkGray)));
            cursor_col = label_end + 4;
        }
        let _ = tab_id;
    }

    // Right-align a small hint.
    let hint = "Ctrl+Tab cycle";
    let left_len: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = width.saturating_sub(left_len + hint.len());
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(hint, Style::default().fg(Color::DarkGray)));

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(rgb(theme.bg_primary))),
        area,
    );
}

// ─── Journal body ─────────────────────────────────────────────────────────────

fn render_journal_body(frame: &mut Frame, area: Rect, snap: &LayoutSnapshot) {
    let theme = &snap.theme;
    let width = area.width as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();

    for entry in &snap.log_snapshot {
        match entry {
            LogEntry::User(text) => {
                let summary: String = text
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(width.saturating_sub(14))
                    .collect();
                lines.push(Line::from(vec![
                    Span::styled("         you  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(summary, Style::default().fg(rgb(theme.accent))),
                ]));
            }
            LogEntry::Assistant(text) => {
                let clean = strip_ansi(text);
                for raw_line in clean.lines().take(5) {
                    let trimmed: String = raw_line.chars().take(width.saturating_sub(14)).collect();
                    lines.push(Line::from(vec![
                        Span::raw("              "),
                        Span::styled(trimmed, Style::default().fg(Color::White)),
                    ]));
                }
            }
            LogEntry::ToolCall { name, detail, full_result, done, .. } => {
                let op = if name.contains("read") || name.contains("Read") {
                    "read"
                } else if name.contains("edit") || name.contains("Edit") || name.contains("write") || name.contains("Write") {
                    "edit"
                } else if name.contains("bash") || name.contains("Bash") || name.contains("exec") {
                    "exec"
                } else {
                    name.as_str()
                };

                let args_str: String = detail
                    .chars()
                    .take(width.saturating_sub(op.len() + 22))
                    .collect();

                let result_indicator = if *done { "✓" } else { "…" };
                let result_color = if full_result.is_some() { Color::Green } else { Color::DarkGray };
                lines.push(Line::from(vec![
                    Span::styled("  •  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{op:<6}"),
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {args_str}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("  {result_indicator}"),
                        Style::default().fg(result_color),
                    ),
                ]));
            }
            LogEntry::System(text) => {
                lines.push(Line::from(Span::styled(
                    format!("  ◆  {text}"),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
            // Inline images are not renderable in text mode; skip.
            LogEntry::Image { .. } => {}
        }
    }

    // Streaming pending text (current assistant turn).
    if !snap.pending.is_empty() {
        let clean = strip_ansi(&snap.pending);
        for raw_line in clean.lines().take(3) {
            let trimmed: String = raw_line.chars().take(width.saturating_sub(14)).collect();
            lines.push(Line::from(vec![
                Span::raw("              "),
                Span::styled(
                    trimmed,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
    }

    // Scroll to bottom: show the last N lines that fit.
    let visible_rows = area.height as usize;
    let start = lines.len().saturating_sub(visible_rows);
    let visible: Vec<Line<'static>> = lines.into_iter().skip(start).collect();

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Paragraph::new(Text::from(visible))
            .style(Style::default().bg(rgb(theme.bg_primary)))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        area,
    );
}

// ─── Ctrl-K palette overlay ───────────────────────────────────────────────────

/// Render the command palette.
///
/// Sources (in order): slash commands, thread names, recent prompts from
/// `Tab.history` (via `queued_preview` for now — full `file_cache` fuzzy index
/// deferred to step 4 since it requires accessing the runtime file cache not yet
/// part of `LayoutSnapshot`).
fn render_palette(
    frame: &mut Frame,
    size: Rect,
    snap: &LayoutSnapshot,
    query: &str,
    selected: usize,
) {
    let theme = &snap.theme;

    // Build items.
    let mut items: Vec<(String, String)> = Vec::new();

    // Slash commands that fuzzy-match the query.
    let slash_cmds: &[(&str, &str)] = &[
        ("/help", "show help"),
        ("/clear", "clear conversation"),
        ("/model", "switch model"),
        ("/layout", "switch TUI layout"),
        ("/status", "session status"),
        ("/config", "configuration"),
        ("/cost", "token cost summary"),
        ("/exit", "exit Anvil"),
    ];
    for (cmd, hint) in slash_cmds {
        if query.is_empty() || cmd.contains(query) || hint.contains(query) {
            items.push((cmd.to_string(), hint.to_string()));
        }
    }

    // Thread names.
    for (_id, name, is_active, _unread, _perm) in &snap.tab_infos {
        let label = format!("Switch thread: {name}");
        if query.is_empty() || name.contains(&query[..]) {
            let hint = if *is_active { "active" } else { "background" };
            items.push((label, hint.to_string()));
        }
    }

    // Recent prompts from queued_preview (step 4 will add full history).
    for preview in &snap.queued_preview {
        if query.is_empty() || preview.contains(query) {
            items.push((preview.clone(), "recent prompt".to_string()));
        }
    }

    let max_visible = 10usize;
    let visible_count = items.len().min(max_visible);
    let palette_h = (visible_count as u16 + 4).min(size.height.saturating_sub(4));
    let palette_w = (size.width / 2).min(70).max(30);
    let palette_x = (size.width.saturating_sub(palette_w)) / 2;
    let palette_y = size.height / 4;

    let palette_area = Rect {
        x: palette_x,
        y: palette_y,
        width: palette_w,
        height: palette_h,
    };

    frame.render_widget(ratatui::widgets::Clear, palette_area);

    // Query line.
    let query_line = Line::from(vec![
        Span::styled(" ❯ ", Style::default().fg(rgb(theme.accent))),
        Span::raw(query.to_string()),
        Span::styled("█", Style::default().fg(Color::White)),
    ]);

    let hint_line = Line::from(Span::styled(
        " ↑↓ navigate  Enter execute  Esc close",
        Style::default().fg(Color::DarkGray),
    ));

    let inner_w = palette_w.saturating_sub(2) as usize;
    let mut palette_lines: Vec<Line<'static>> = vec![query_line];

    for (i, (cmd, hint)) in items.iter().take(max_visible).enumerate() {
        let is_sel = i == selected;
        let (fg, bg) = if is_sel {
            (rgb(theme.bg_primary), rgb(theme.accent))
        } else {
            (rgb(theme.text_primary), rgb(theme.bg_card))
        };
        let cmd_w = (inner_w / 2).min(cmd.len() + 4);
        let padded = format!(" {cmd:<width$}", width = cmd_w);
        let hint_w = inner_w.saturating_sub(cmd_w + 1);
        let padded_hint: String = format!("{hint}").chars().take(hint_w).collect();
        palette_lines.push(Line::from(vec![
            Span::styled(padded, Style::default().fg(fg).bg(bg)),
            Span::styled(
                format!("  {padded_hint}"),
                Style::default().fg(if is_sel { rgb(theme.bg_primary) } else { rgb(theme.text_secondary) }).bg(bg)
                    .add_modifier(Modifier::DIM),
            ),
        ]));
    }
    palette_lines.push(hint_line);

    frame.render_widget(
        Paragraph::new(Text::from(palette_lines))
            .block(
                ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_style(Style::default().fg(rgb(theme.border)))
                    .style(Style::default().bg(rgb(theme.bg_card))),
            ),
        palette_area,
    );
}
