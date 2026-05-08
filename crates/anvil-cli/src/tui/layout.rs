/// Ratatui layout calculation, constraint definitions, area splitting,
/// and status-line span builders.
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use runtime::theme::{Rgb, StatusLineConfig, StatusWidget};

// ─── Input line count ─────────────────────────────────────────────────────────

/// Calculate how many terminal rows the input text will occupy (1–5).
///
/// The first visual row is prefixed by `"❯ "` (2 columns), so its usable
/// width is `width.saturating_sub(2)`.  Continuation rows start at column 0
/// with no indent, giving full `width` usable columns.  Both literal `\n`
/// characters (Ctrl+J newlines) and soft-wrap boundaries are counted.
pub(super) fn compute_input_lines(input: &str, width: usize) -> usize {
    if width < 4 {
        return 1;
    }
    let prompt_width: usize = 2; // "❯ "
    let first_col = width.saturating_sub(prompt_width).max(1);
    let rest_col = width.max(1);

    let mut total_rows: usize = 0;
    let logical_lines: Vec<&str> = input.split('\n').collect();
    let n_logical = logical_lines.len();

    for (idx, logical_line) in logical_lines.iter().enumerate() {
        let avail_first = if total_rows == 0 { first_col } else { rest_col };
        let char_count = logical_line.chars().count();

        if char_count == 0 {
            total_rows += 1;
        } else if char_count <= avail_first {
            total_rows += 1;
        } else {
            let remaining = char_count - avail_first;
            let extra = remaining.div_ceil(rest_col);
            total_rows += 1 + extra;
        }

        if total_rows >= 5 {
            return 5;
        }
        let _ = (idx, n_logical);
    }

    total_rows.clamp(1, 5)
}

// ─── Cursor position ──────────────────────────────────────────────────────────

/// Cursor position (visual row offset from footer line 1, column) for the
/// terminal cursor indicator.
///
/// Returns `(row_offset, col)` where `row_offset` is 0-based within the input
/// area (0 = first input row which is footer row 1).
pub(super) fn cursor_visual_position(input: &str, cursor_pos: usize, width: usize) -> (usize, usize) {
    if width < 4 {
        return (0, 2);
    }
    let prompt_width: usize = 2;
    let first_col = width.saturating_sub(prompt_width).max(1);
    let rest_col = width.max(1);

    let mut row: usize = 0;
    let mut col: usize = 0;
    let mut byte_offset: usize = 0;

    let logical_lines: Vec<&str> = input.split('\n').collect();
    let n_logical = logical_lines.len();

    'outer: for (lidx, logical_line) in logical_lines.iter().enumerate() {
        let mut col_in_row: usize = 0;

        for ch in logical_line.chars() {
            if byte_offset == cursor_pos {
                col = col_in_row;
                break 'outer;
            }
            let avail_now = if row == 0 { first_col } else { rest_col };
            if col_in_row >= avail_now {
                row += 1;
                col_in_row = 0;
            }
            byte_offset += ch.len_utf8();
            col_in_row += 1;
        }

        if byte_offset == cursor_pos {
            col = col_in_row;
            break 'outer;
        }

        if lidx + 1 < n_logical {
            byte_offset += 1; // '\n'
            row += 1;
        }
    }

    let visual_col = if row == 0 { col + prompt_width } else { col };
    (row, visual_col)
}

// ─── Status line builders ─────────────────────────────────────────────────────

/// Build a ratatui `Line` with left-aligned spans and right-aligned spans,
/// padding the middle with spaces to fill the terminal width.
pub(super) fn build_left_right_line(
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

// ─── Dynamic status line widget system ───────────────────────────────────────

/// Convert a runtime `Rgb` triple into a ratatui `Color`.
#[inline]
const fn to_color(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// All live data that widgets may need to render.
pub(super) struct StatusLineData {
    pub model: String,
    pub thinking_enabled: bool,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: String,
    pub context_used: u32,
    pub context_max: u32,
    pub elapsed_secs: u64,
    pub git_branch: String,
    pub git_diff: String,
    pub git_clean: bool,
    pub permission_mode: String,
    pub qmd_status: String,
    pub archive_status: String,
    pub update_available: String,
    pub remote_url: String,
    pub remote_code: String,
    pub vim_mode: bool,
    pub version: String,
    pub provider: String,
    pub token_speed: f64,
    // Extended metrics
    pub burn_rate_hr: f64,
    pub cost_daily: f64,
    pub cost_weekly: f64,
    pub cost_monthly: f64,
    pub cache_hit_pct: f64,
    pub lines_added: u32,
    pub lines_removed: u32,
    pub mcp_server_count: u32,
    /// Current effort level, e.g. "medium" or "high".
    pub effort_level: String,
    // Theme colors
    pub accent: Rgb,
    pub warning: Rgb,
    #[allow(dead_code)]
    pub success: Rgb,
    #[allow(dead_code)]
    pub error: Rgb,
}

/// Render a single widget into spans.
fn render_widget(
    widget: &StatusWidget,
    data: &StatusLineData,
    separator_char: &str,
) -> Vec<Span<'static>> {
    let gray = Color::Rgb(0x88, 0x88, 0x88);
    let dim = Style::default().fg(gray);

    match widget {
        StatusWidget::Model => vec![
            Span::styled("Model: ", dim),
            Span::styled(data.model.clone(), Style::default().fg(Color::Yellow)),
        ],
        StatusWidget::Thinking => {
            let (text, color) = if data.thinking_enabled {
                ("Yes".to_string(), Color::Green)
            } else {
                ("No".to_string(), gray)
            };
            vec![
                Span::styled("Thinking: ", dim),
                Span::styled(text, Style::default().fg(color)),
            ]
        }
        StatusWidget::Effort => {
            let (text, color) = if data.effort_level == "medium" || data.effort_level.is_empty() {
                (data.effort_level.as_str().to_string(), gray)
            } else {
                (data.effort_level.as_str().to_string(), Color::Green)
            };
            vec![
                Span::styled("Effort: ", dim),
                Span::styled(text, Style::default().fg(color)),
            ]
        }
        StatusWidget::Provider => vec![
            Span::styled("Provider: ", dim),
            Span::styled(data.provider.clone(), Style::default().fg(to_color(data.accent))),
        ],
        StatusWidget::TokensTotal => {
            let total = data.input_tokens.saturating_add(data.output_tokens);
            vec![Span::styled(format!("{total} tokens"), dim)]
        }
        StatusWidget::TokensInput => {
            vec![Span::styled(format!("In: {}", data.input_tokens), dim)]
        }
        StatusWidget::TokensOutput => {
            vec![Span::styled(format!("Out: {}", data.output_tokens), dim)]
        }
        StatusWidget::Cost => {
            vec![Span::styled(
                format!("Cost: {}", data.cost_usd),
                Style::default().fg(Color::Rgb(0x88, 0xcc, 0x88)),
            )]
        }
        StatusWidget::TokenSpeed => {
            if data.token_speed > 0.0 {
                vec![Span::styled(format!("{:.0} t/s", data.token_speed), dim)]
            } else {
                vec![]
            }
        }
        StatusWidget::ContextBar => {
            let bar_width: usize = 16;
            let pct = if data.context_max > 0 {
                ((f64::from(data.context_used) / f64::from(data.context_max)) * 100.0).min(100.0)
            } else {
                0.0
            };
            let filled = ((pct / 100.0) * bar_width as f64).round() as usize;
            let empty = bar_width.saturating_sub(filled);
            let bar_color = if pct >= 95.0 {
                Color::Red
            } else if pct >= 80.0 {
                Color::Yellow
            } else {
                Color::Blue
            };
            vec![
                Span::raw("Context: ["),
                Span::styled("█".repeat(filled), Style::default().fg(bar_color)),
                Span::styled("░".repeat(empty), Style::default().fg(Color::Rgb(0x33, 0x33, 0x33))),
                Span::raw("] "),
            ]
        }
        StatusWidget::ContextPct => {
            let pct = if data.context_max > 0 {
                ((f64::from(data.context_used) / f64::from(data.context_max)) * 100.0).min(100.0)
            } else {
                0.0
            };
            let mut spans = vec![Span::styled(
                format!("{pct:.0}%"),
                Style::default().fg(Color::Yellow),
            )];
            if pct >= 95.0 {
                spans.push(Span::styled(
                    " ⚠ CRITICAL",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ));
            } else if pct >= 80.0 {
                spans.push(Span::styled(" ⚠ high", Style::default().fg(Color::Yellow)));
            }
            spans
        }
        StatusWidget::ContextTokens => {
            let used_k = data.context_used / 1000;
            let max_k = data.context_max / 1000;
            vec![Span::styled(
                format!("{used_k}k/{max_k}k"),
                Style::default().fg(Color::Yellow),
            )]
        }
        StatusWidget::SessionTime => {
            let secs = data.elapsed_secs;
            let dur = if secs < 3600 {
                format!("{}m{}s", secs / 60, secs % 60)
            } else {
                format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
            };
            vec![Span::styled(format!("Session: {dur}"), dim)]
        }
        StatusWidget::SessionPct => {
            let pct = if data.context_max > 0 {
                ((f64::from(data.context_used) / f64::from(data.context_max)) * 100.0).min(100.0)
            } else {
                0.0
            };
            vec![Span::styled(format!("Session: {pct:.1}%"), dim)]
        }
        StatusWidget::BlockTime => {
            let secs = data.elapsed_secs;
            let dur = if secs < 3600 {
                format!("{}m", secs / 60)
            } else {
                format!("{}hr", secs / 3600)
            };
            vec![Span::styled(format!("Block: {dur}"), dim)]
        }
        StatusWidget::GitBranch => {
            if data.git_branch.is_empty() {
                vec![]
            } else {
                vec![
                    Span::styled("⌐", dim),
                    Span::styled(data.git_branch.clone(), Style::default().fg(Color::Green)),
                ]
            }
        }
        StatusWidget::GitStatus => {
            if data.git_branch.is_empty() {
                vec![]
            } else {
                let (label, color) = if data.git_clean {
                    ("clean", Color::Green)
                } else {
                    ("dirty", Color::Yellow)
                };
                vec![Span::styled(label.to_string(), Style::default().fg(color))]
            }
        }
        StatusWidget::GitDiff => {
            if data.git_diff.is_empty() {
                vec![]
            } else {
                vec![Span::styled(format!("({})", data.git_diff), dim)]
            }
        }
        StatusWidget::Permissions => {
            vec![
                Span::styled("▸▸ ", Style::default().fg(to_color(data.warning))),
                Span::styled(
                    data.permission_mode.clone(),
                    Style::default().fg(to_color(data.warning)).add_modifier(Modifier::DIM),
                ),
            ]
        }
        StatusWidget::QmdStatus => {
            if data.qmd_status.is_empty() {
                vec![]
            } else {
                vec![Span::styled(
                    format!("📚 {}", data.qmd_status),
                    Style::default().fg(Color::Rgb(0x55, 0x88, 0x55)),
                )]
            }
        }
        StatusWidget::Version => {
            vec![Span::styled(
                format!("v{}", data.version),
                Style::default().fg(Color::Rgb(0x66, 0x66, 0x66)),
            )]
        }
        StatusWidget::VimMode => {
            if data.vim_mode {
                vec![Span::styled("VIM", Style::default().fg(Color::Green))]
            } else {
                vec![]
            }
        }
        StatusWidget::RemoteControl => {
            // remote_url = pairing code, remote_code = client status
            if data.remote_url.is_empty() {
                vec![]
            } else {
                let code = &data.remote_url;
                let clients = &data.remote_code;
                let color = if clients == "waiting" || clients.is_empty() {
                    Color::Yellow
                } else if clients == "0 clients" {
                    Color::Rgb(0xF5, 0x9E, 0x0B) // warning amber
                } else {
                    Color::Rgb(0x55, 0xCC, 0xFF) // connected cyan
                };
                let label = if clients == "waiting" || clients.is_empty() {
                    format!("\u{1f6f8} RC [{code}, waiting]")
                } else {
                    format!("\u{1f6f8} RC [{code}, {clients}]")
                };
                vec![Span::styled(
                    label,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                )]
            }
        }
        StatusWidget::UpdateAvailable => {
            if data.update_available.is_empty() {
                vec![]
            } else {
                vec![Span::styled(
                    format!("⬆ {}", data.update_available),
                    Style::default()
                        .fg(Color::Rgb(0xFF, 0xAA, 0x00))
                        .add_modifier(Modifier::BOLD),
                )]
            }
        }
        StatusWidget::ArchiveStatus => {
            if data.archive_status.is_empty() {
                vec![]
            } else {
                vec![Span::styled(
                    format!("📦 {}", data.archive_status),
                    Style::default().fg(Color::Rgb(0x55, 0x77, 0xAA)),
                )]
            }
        }
        StatusWidget::McpStatus => {
            if data.mcp_server_count > 0 {
                vec![Span::styled(
                    format!("🔌 MCP: {}", data.mcp_server_count),
                    Style::default().fg(Color::Rgb(0x55, 0xCC, 0xFF)),
                )]
            } else {
                vec![]
            }
        }
        StatusWidget::TimeDisplay => {
            // Current wall-clock time
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let hours = (now % 86400) / 3600;
            let minutes = (now % 3600) / 60;
            vec![Span::styled(
                format!("🕐 {hours:02}:{minutes:02}"),
                dim,
            )]
        }
        StatusWidget::BurnRate => {
            if data.burn_rate_hr > 0.0 {
                let color = if data.burn_rate_hr > 5.0 {
                    Color::Red
                } else if data.burn_rate_hr > 2.0 {
                    Color::Yellow
                } else {
                    Color::Rgb(0x88, 0xcc, 0x88)
                };
                vec![Span::styled(
                    format!("🔥 ${:.2}/hr", data.burn_rate_hr),
                    Style::default().fg(color),
                )]
            } else {
                vec![]
            }
        }
        StatusWidget::CostDaily => {
            if data.cost_daily > 0.0 {
                vec![Span::styled(
                    format!("💰 Day: ${:.2}", data.cost_daily),
                    Style::default().fg(Color::Rgb(0x88, 0xcc, 0x88)),
                )]
            } else {
                vec![]
            }
        }
        StatusWidget::CostWeekly => {
            if data.cost_weekly > 0.0 {
                vec![Span::styled(
                    format!("💰 Week: ${:.2}", data.cost_weekly),
                    Style::default().fg(Color::Rgb(0x88, 0xcc, 0x88)),
                )]
            } else {
                vec![]
            }
        }
        StatusWidget::CostMonthly => {
            if data.cost_monthly > 0.0 {
                vec![Span::styled(
                    format!("💰 Month: ${:.2}", data.cost_monthly),
                    Style::default().fg(Color::Rgb(0x88, 0xcc, 0x88)),
                )]
            } else {
                vec![]
            }
        }
        StatusWidget::CostProjection => {
            if data.burn_rate_hr > 0.0 {
                let projected = data.burn_rate_hr * (data.elapsed_secs as f64 / 3600.0) * 2.0;
                vec![Span::styled(
                    format!("📈 Est: ${projected:.2}"),
                    Style::default().fg(Color::Rgb(0xCC, 0xAA, 0x55)),
                )]
            } else {
                vec![]
            }
        }
        StatusWidget::CacheHitRate => {
            if data.cache_hit_pct > 0.0 {
                let color = if data.cache_hit_pct >= 80.0 {
                    Color::Green
                } else if data.cache_hit_pct >= 50.0 {
                    Color::Yellow
                } else {
                    Color::Red
                };
                vec![Span::styled(
                    format!("Cache: {:.0}%", data.cache_hit_pct),
                    Style::default().fg(color),
                )]
            } else {
                vec![]
            }
        }
        StatusWidget::CodeProductivity => {
            if data.lines_added > 0 || data.lines_removed > 0 {
                vec![Span::styled(
                    format!("📝 +{}/-{} lines", data.lines_added, data.lines_removed),
                    Style::default().fg(Color::Rgb(0x88, 0xAA, 0xCC)),
                )]
            } else {
                vec![]
            }
        }
        StatusWidget::Text { content } => {
            vec![Span::styled(content.clone(), dim)]
        }
        StatusWidget::Spacer => {
            // Spacers are handled at the line-composition level
            vec![]
        }
        StatusWidget::Separator => {
            vec![Span::styled(separator_char.to_string(), dim)]
        }
    }
}

/// Render all status lines from a `StatusLineConfig` into ratatui `Line`s.
pub(super) fn render_status_lines(
    config: &StatusLineConfig,
    data: &StatusLineData,
    width: usize,
) -> Vec<Line<'static>> {
    let sep = &config.separator_char;
    let mut result = Vec::with_capacity(config.lines.len());

    for line_cfg in &config.lines {
        let mut left_spans: Vec<Span<'static>> = Vec::new();
        for widget in &line_cfg.left {
            let rendered = render_widget(widget, data, sep);
            if rendered.is_empty() {
                continue;
            }
            // Add separator spacing between non-empty widgets (except if this IS a separator)
            if !left_spans.is_empty() && widget.id() != "separator" {
                // Check if previous widget was a separator — if so, skip auto-spacing
                let prev_is_sep = left_spans
                    .last()
                    .is_some_and(|s| s.content.contains('│') || s.content.contains('·'));
                if !prev_is_sep {
                    left_spans.push(Span::raw(" "));
                }
            }
            left_spans.extend(rendered);
        }

        let mut right_spans: Vec<Span<'static>> = Vec::new();
        for widget in &line_cfg.right {
            let rendered = render_widget(widget, data, sep);
            if rendered.is_empty() {
                continue;
            }
            if !right_spans.is_empty() && widget.id() != "separator" {
                right_spans.push(Span::raw(" "));
            }
            right_spans.extend(rendered);
        }

        result.push(build_left_right_line(left_spans, right_spans, width));
    }

    result
}
