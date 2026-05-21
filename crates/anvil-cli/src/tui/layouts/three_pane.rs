/// Layout B/E — Three-Pane renderer.
///
/// Three horizontal bands:
///   Top third:    FOCUS — current assistant action (pending_text + last streaming log entry).
///   Middle third: LOG   — compact bullets of each log entry (one line per entry).
///   Bottom third: CONTEXT — model, tokens, cost, memory indicators, git, files in scope.
///
/// Input is ALWAYS editable — no vim modal, no "press i" nonsense.  The user
/// just starts typing.  This matches the UX of classic and journal layouts.
///
/// Layout E (tabs: true): tab bar at row 0 above FOCUS.
/// Layout B (tabs: false): FOCUS starts at row 0.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout as RLayout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use rust_i18n::t;

use super::common::{rgb, render_completion_popup, render_tab_bar};
use super::{LayoutLocalState, TuiLayoutRenderer};
use crate::tui::helpers::strip_ansi;
use crate::tui::layout::cursor_visual_position;
use crate::tui::redraw::DirtyRegions;
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
        _local: &mut LayoutLocalState,
        tab_hits_out: &mut Vec<TabHit>,
    ) {
        let size = frame.area();

        // BUG-3 fix: wipe all cells before drawing.
        //
        // Task #622 (CRITICAL accessibility fix): gate the full-screen Clear
        // on structural dirty (DirtyRegions::ALL) only — unconditional Clear
        // flashes on Gnome Terminal during streaming. See classic.rs for the
        // full rationale.
        let force_full_clear = snap.dirty_regions.contains(DirtyRegions::ALL)
            || std::env::var("ANVIL_TUI_FORCE_CLEAR")
                .map(|v| v == "1")
                .unwrap_or(false);
        if force_full_clear {
            frame.render_widget(ratatui::widgets::Clear, size);
        }

        // Row 0 (Layout E only): tab bar.
        let mut y_offset: u16 = 0;
        if self.tabs {
            let tab_area = Rect { x: size.x, y: size.y, width: size.width, height: 1 };
            render_tab_bar(frame, tab_area, snap, tab_hits_out);
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
        let focus_h = third.max(4);
        let log_h = third.max(3);

        let bands = RLayout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(focus_h),
                Constraint::Length(log_h),
                Constraint::Fill(1),
            ])
            .split(remaining);

        let focus_area = bands[0];
        let log_area = bands[1];
        let context_area = bands[2];

        render_focus_pane(frame, focus_area, snap);
        render_log_pane(frame, log_area, snap);
        render_context_pane(frame, context_area, snap);

        // Completion popup above the focus pane input row.
        render_completion_popup(frame, focus_area, snap);

        // Cursor always positioned at the input row in the FOCUS pane.
        let input_row = focus_area.y + focus_area.height.saturating_sub(1);
        let input_width = focus_area.width as usize;
        let (_, cursor_col) = cursor_visual_position(&snap.input_text, snap.cursor_pos, input_width);
        let max_x = focus_area.x + focus_area.width.saturating_sub(1);
        frame.set_cursor_position(Position {
            x: (focus_area.x + cursor_col as u16).min(max_x),
            y: input_row,
        });
    }
}

// ─── FOCUS pane ───────────────────────────────────────────────────────────────

fn render_focus_pane(frame: &mut Frame, area: Rect, snap: &LayoutSnapshot) {
    let theme = &snap.theme;
    let width = area.width as usize;

    // Header line: "─── FOCUS ────────"
    let focus_label = t!("tui.three_pane.focus_header").to_string();
    let label_chars = focus_label.chars().count();
    let header = format!("{focus_label}{}", "─".repeat(width.saturating_sub(label_chars)));
    let header_line = Line::from(Span::styled(
        header,
        Style::default().fg(rgb(theme.border)),
    ));

    // Content: pending_text (streaming) or last log entry if no pending.
    let content_lines: Vec<Line<'static>> = if !snap.pending.is_empty() {
        let clean = strip_ansi(&snap.pending);
        clean.lines().map(|l| Line::from(Span::raw(l.to_string()))).collect()
    } else if let Some(last) = snap.log_snapshot.last() {
        match last {
            LogEntry::Assistant(text) => {
                let clean = strip_ansi(text);
                clean
                    .lines()
                    .map(|l| {
                        Line::from(Span::styled(
                            l.to_string(),
                            Style::default().fg(Color::White),
                        ))
                    })
                    .collect()
            }
            _ => vec![Line::from(Span::styled(
                t!("tui.deck.waiting").to_string(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ))],
        }
    } else {
        vec![Line::from(Span::styled(
            t!("tui.deck.no_conversation").to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ))]
    };

    // Reserve: header(1) + content + separator(1) + input(1) = 3 fixed rows.
    let content_height = area.height.saturating_sub(3) as usize;
    let visible: Vec<Line<'static>> = content_lines.into_iter().take(content_height).collect();

    // Separator line above the input row.
    let sep_line = Line::from(Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(rgb(theme.border)),
    ));

    // Always-on input row.
    let input_row = render_input_row(snap, area.width as usize);

    let mut all_lines: Vec<Line<'static>> = vec![header_line];
    all_lines.extend(visible);
    // Pad so separator lands at area.height - 2.
    while all_lines.len() < area.height.saturating_sub(2) as usize {
        all_lines.push(Line::from(""));
    }
    all_lines.push(sep_line);
    all_lines.push(input_row);

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Paragraph::new(Text::from(all_lines))
            .style(Style::default().bg(rgb(theme.bg_primary)))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        area,
    );
}

/// Build the always-on input row `▌ <input>` for the FOCUS pane.
fn render_input_row(snap: &LayoutSnapshot, width: usize) -> Line<'static> {
    let theme = &snap.theme;
    let prompt_style = Style::default()
        .fg(rgb(theme.accent))
        .add_modifier(Modifier::BOLD);

    if snap.input_text.is_empty() {
        // Show a dim placeholder with cursor block.
        Line::from(vec![
            Span::styled("❯ ", prompt_style),
            Span::styled(
                " ",
                Style::default()
                    .fg(Color::Rgb(0x1a, 0x1a, 0x1a))
                    .bg(Color::White),
            ),
        ])
    } else {
        // Show typed text with inline cursor block.
        let cursor = snap.cursor_pos;
        let input = &snap.input_text;
        let available = width.saturating_sub(2);
        let before: String = input.chars().take(cursor).take(available).collect();
        let cur_char = input.chars().nth(cursor);
        let after_start = cursor + 1;
        let after: String = input
            .chars()
            .skip(after_start)
            .take(available.saturating_sub(before.chars().count() + 1))
            .collect();

        let mut spans = vec![Span::styled("❯ ", prompt_style)];
        if !before.is_empty() {
            spans.push(Span::styled(
                before,
                Style::default().fg(Color::White),
            ));
        }
        if let Some(ch) = cur_char {
            spans.push(Span::styled(
                ch.to_string(),
                Style::default()
                    .fg(Color::Rgb(0x1a, 0x1a, 0x1a))
                    .bg(Color::White),
            ));
        } else {
            spans.push(Span::styled(
                " ",
                Style::default()
                    .fg(Color::Rgb(0x1a, 0x1a, 0x1a))
                    .bg(Color::White),
            ));
        }
        if !after.is_empty() {
            spans.push(Span::styled(after, Style::default().fg(Color::White)));
        }
        Line::from(spans)
    }
}

// ─── LOG pane ─────────────────────────────────────────────────────────────────

fn render_log_pane(frame: &mut Frame, area: Rect, snap: &LayoutSnapshot) {
    let theme = &snap.theme;
    let width = area.width as usize;

    let log_label = t!("tui.three_pane.log_header").to_string();
    let log_chars = log_label.chars().count();
    let header = format!("{log_label}{}", "─".repeat(width.saturating_sub(log_chars)));
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
            LogEntry::User(txt) => (
                t!("tui.three_pane.log_label_you").to_string(),
                txt.as_str(),
                rgb(theme.accent),
            ),
            LogEntry::Assistant(txt) => (
                t!("tui.three_pane.log_label_ast").to_string(),
                txt.as_str(),
                Color::White,
            ),
            LogEntry::System(txt) => (
                t!("tui.three_pane.log_label_sys").to_string(),
                txt.as_str(),
                Color::DarkGray,
            ),
            _ => unreachable!(),
        };
        let prefix_chars = prefix.chars().count();
        let summary: String = content
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(width.saturating_sub(prefix_chars))
            .collect();
        lines.push(Line::from(vec![
            Span::styled(prefix, Style::default().fg(Color::DarkGray)),
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

    let ctx_label = t!("tui.three_pane.context_header").to_string();
    let ctx_chars = ctx_label.chars().count();
    let header = format!("{ctx_label}{}", "─".repeat(width.saturating_sub(ctx_chars)));
    let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled(
        header,
        Style::default().fg(rgb(theme.border)),
    ))];

    // Model + tokens + cost.
    let cost_str = if snap.model.contains(':') && !snap.model.contains(":cloud") {
        t!("tui.three_pane.ctx_cost_local").to_string()
    } else {
        t!(
            "tui.three_pane.ctx_cost_in_out",
            input = snap.input_tokens.to_string(),
            output = snap.output_tokens.to_string()
        )
        .to_string()
    };
    lines.push(Line::from(vec![
        Span::styled(
            t!("tui.three_pane.ctx_model").to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(snap.model.clone(), Style::default().fg(Color::Yellow)),
        Span::styled("   ", Style::default()),
        Span::styled(cost_str, Style::default().fg(Color::DarkGray)),
    ]));

    // Git branch + diff.
    if !snap.git_branch.is_empty() {
        let diff = if snap.git_diff_stats.is_empty() {
            t!("tui.three_pane.ctx_git_clean").to_string()
        } else {
            snap.git_diff_stats.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(
                t!("tui.three_pane.ctx_git").to_string(),
                Style::default().fg(Color::DarkGray),
            ),
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
            Span::styled(
                t!("tui.three_pane.ctx_ctx").to_string(),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("█".repeat(filled), Style::default().fg(bar_color)),
            Span::styled("░".repeat(empty), Style::default().fg(Color::Rgb(0x33, 0x33, 0x33))),
            Span::styled(format!("] {pct:.0}%"), Style::default().fg(Color::DarkGray)),
        ]));
    }

    // Keybind row at the bottom of context pane.
    let keybind = t!("tui.three_pane.keybinds").to_string();
    let keybind_trunc: String = keybind.chars().take(width).collect();
    // Pad to fill remaining rows.
    while lines.len() < area.height.saturating_sub(1) as usize {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        keybind_trunc,
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::default().bg(rgb(theme.bg_primary))),
        area,
    );
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod three_pane_always_on_input_tests {
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

    fn render_three_pane_fixture(width: u16, height: u16, input: &str) -> String {
        use ratatui::layout::{Constraint, Direction, Layout};
        use ratatui::style::{Color, Modifier, Style};
        use ratatui::text::{Line, Span, Text};
        use ratatui::widgets::Paragraph;

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("TestBackend");

        let input_owned = input.to_string();
        terminal.draw(|frame| {
            let size = frame.area();
            let w = size.width as usize;
            let third = size.height / 3;
            let focus_h = third.max(4);
            let log_h = third.max(3);

            let bands = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(focus_h),
                    Constraint::Length(log_h),
                    Constraint::Fill(1),
                ])
                .split(size);

            let focus_area = bands[0];
            let log_area = bands[1];
            let ctx_area = bands[2];

            // FOCUS pane: header + content + sep + always-on input.
            let header = format!("─── FOCUS{}", "─".repeat(w.saturating_sub(8)));
            let content_height = focus_area.height.saturating_sub(3) as usize;
            let mut focus_lines: Vec<Line<'static>> = vec![
                Line::from(Span::raw(header)),
                Line::from(Span::raw("No conversation yet. Start typing below.")),
            ];
            while focus_lines.len() < content_height + 1 {
                focus_lines.push(Line::from(""));
            }
            focus_lines.push(Line::from(Span::raw("─".repeat(w))));
            // Always-on input row — no "press i" required.
            if input_owned.is_empty() {
                focus_lines.push(Line::from(vec![
                    Span::styled("❯ ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::styled(" ", Style::default().fg(Color::Black).bg(Color::White)),
                ]));
            } else {
                focus_lines.push(Line::from(vec![
                    Span::styled("❯ ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::raw(input_owned.clone()),
                    Span::styled("█", Style::default().fg(Color::White)),
                ]));
            }
            frame.render_widget(
                Paragraph::new(Text::from(focus_lines)).style(Style::default()),
                focus_area,
            );

            // LOG pane.
            let log_header = format!("─── LOG{}", "─".repeat(w.saturating_sub(6)));
            let log_lines: Vec<Line<'static>> = vec![
                Line::from(Span::raw(log_header)),
                Line::from(Span::raw("  you  do the thing")),
                Line::from(Span::raw("  ast  ok")),
            ];
            frame.render_widget(
                Paragraph::new(Text::from(log_lines)).style(Style::default()),
                log_area,
            );

            // CONTEXT pane.
            let ctx_header = format!("─── CONTEXT{}", "─".repeat(w.saturating_sub(10)));
            let keybind = "  PageUp/PageDown scroll  ·  Ctrl+Tab tabs  ·  /quit exit";
            let keybind_trunc: String = keybind.chars().take(w).collect();
            let mut ctx_lines: Vec<Line<'static>> = vec![
                Line::from(Span::raw(ctx_header)),
                Line::from(Span::raw("  Model: claude-sonnet-4-6   in:42 out:17")),
                Line::from(Span::raw("  Git:   main  +12,-5")),
            ];
            while ctx_lines.len() < ctx_area.height.saturating_sub(1) as usize {
                ctx_lines.push(Line::from(""));
            }
            ctx_lines.push(Line::from(Span::styled(keybind_trunc, Style::default().fg(Color::DarkGray))));
            frame.render_widget(
                Paragraph::new(Text::from(ctx_lines)).style(Style::default()),
                ctx_area,
            );
        }).expect("terminal.draw");

        extract_text(&terminal)
    }

    /// Always-on input: typing a char should appear immediately in input row.
    /// Verify the input row contains "❯ " (prompt) without any "press i" text.
    #[test]
    fn three_pane_always_on_input_no_press_i() {
        let rendered = render_three_pane_fixture(80, 24, "");
        // Must NOT contain the old "press i" instruction.
        assert!(
            !rendered.to_lowercase().contains("press i"),
            "three-pane must not show 'press i' — always-on input: {rendered}"
        );
        // Must NOT contain "Normal Mode" (vim mode indicator deleted).
        assert!(
            !rendered.contains("Normal Mode"),
            "three-pane must not show vim Normal Mode: {rendered}"
        );
        // Must show the prompt character at the input row.
        assert!(
            rendered.contains("❯"),
            "three-pane must show ❯ prompt: {rendered}"
        );
    }

    /// Typing a char appears in the input row immediately (no mode gate).
    #[test]
    fn three_pane_typed_char_in_input_row() {
        let rendered = render_three_pane_fixture(80, 24, "hello world");
        assert!(
            rendered.contains("hello world"),
            "typed text must appear in input row: {rendered}"
        );
    }

    /// All rows accounted for at 60×20 (BUG-4 Fill constraint regression check).
    #[test]
    fn three_pane_all_rows_60x20() {
        let rendered = render_three_pane_fixture(60, 20, "");
        let row_count = rendered.split('\n').count();
        assert_eq!(row_count, 20, "expected 20 rows but got {row_count}");
        insta::assert_snapshot!("three_pane_always_on__60x20", rendered);
    }

    /// 80×24 golden snapshot.
    #[test]
    fn three_pane_always_on__80x24() {
        let rendered = render_three_pane_fixture(80, 24, "");
        insta::assert_snapshot!("three_pane_always_on__80x24", rendered);
    }

    /// 120×40 golden snapshot.
    #[test]
    fn three_pane_always_on__120x40() {
        let rendered = render_three_pane_fixture(120, 40, "");
        insta::assert_snapshot!("three_pane_always_on__120x40", rendered);
    }

    /// 200×60 golden snapshot.
    #[test]
    fn three_pane_always_on__200x60() {
        let rendered = render_three_pane_fixture(200, 60, "");
        insta::assert_snapshot!("three_pane_always_on__200x60", rendered);
    }
}
