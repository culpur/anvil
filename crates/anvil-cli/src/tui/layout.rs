/// Ratatui layout calculation, constraint definitions, area splitting,
/// and status-line span builders.
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

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

    total_rows.max(1).min(5)
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

/// Build the left spans for status line 1 with model name in yellow,
/// git branch in green, and diff stats in dim white.
pub(super) fn build_status1_spans(
    model: &str,
    total_m: f64,
    git_branch: &str,
    git_diff: &str,
    cost_usd: &str,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled("Model: ", Style::default().fg(Color::Rgb(0x88, 0x88, 0x88))),
        Span::styled(model.to_string(), Style::default().fg(Color::Yellow)),
        Span::styled(
            format!(" | Total: {total_m:.1}M"),
            Style::default().fg(Color::Rgb(0x88, 0x88, 0x88)),
        ),
        Span::styled(
            format!(" | Cost: {cost_usd}"),
            Style::default().fg(Color::Rgb(0x88, 0xcc, 0x88)),
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
