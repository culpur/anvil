//! `TextareaModal` — multi-line textarea overlay (task #684).
//!
//! Replaces the single-line `TextInputModal` for fields where users may
//! write 200–500 word descriptions — the primary case being the `/mcp
//! builder` AI-spec-generation prompt.
//!
//! ## Layout
//!
//! ```text
//! ╭─ MCP description ─────────────────────────────────────────────╮
//! │                                                               │
//! │  Prompt text shown as ghost hint when buffer is empty         │
//! │                                                               │
//! │  [line 1 of user text, word-wrapped to inner width]          │
//! │  [continuation of line 1 if it wraps]                        │
//! │  [line 2]                                                     │
//! │  [cursor]                                                      │
//! │                                                               │
//! │  ─────────────────────────────────── 0 words · 0 chars       │
//! │  Enter: newline   Ctrl+Enter/Alt+Enter: submit   Esc: cancel  │
//! ╰───────────────────────────────────────────────────────────────╯
//! ```
//!
//! ## Keys
//!
//! - Printable chars                insert at cursor
//! - Enter                          insert newline
//! - Ctrl+Enter / Alt+Enter         submit
//! - Esc                            cancel (returns `Cancel(String)`)
//! - Ctrl+C                         cancel
//! - Backspace                      delete char before cursor (crosses
//!                                  line boundaries)
//! - Delete                         delete char after cursor
//! - Left / Right                   move cursor
//! - Up / Down                      move cursor to same column on
//!                                  previous / next visual line
//! - Home                           jump to start of logical line
//! - End                            jump to end of logical line
//! - Ctrl+U                         clear buffer
//! - Paste (bracketed)              insert pasted text (newlines
//!                                  preserved)
//!
//! ## Scroll
//!
//! The visible area is capped at `max_visible_rows` (default 15). When
//! the content exceeds that height the view scrolls so the cursor row is
//! always visible, with a 1-row look-ahead margin at the bottom.
//!
//! ## Word-wrap
//!
//! The buffer stores the raw user text as a single `String`. Newlines
//! typed by the user are `'\n'`; the renderer wraps logical lines that
//! exceed the inner column width into multiple visual rows. Cursor
//! movement navigates visual rows (including wrapped continuations) so
//! Up/Down feel natural.
//!
//! ## TUI discipline
//!
//! Per `feedback-tui-stdout-anti-pattern.md` and the clippy gate on this
//! crate, this file carries `#![deny(clippy::print_stdout,
//! clippy::print_stderr)]` and all output goes through the ratatui frame
//! buffer. The modal is region-scoped — `Clear` is applied only to the
//! modal's own `Rect` (never the full screen), per
//! `feedback-tui-flash-anti-pattern.md`.

#![deny(clippy::print_stdout, clippy::print_stderr)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

// ─── Public action enum ───────────────────────────────────────────────────────

/// Outcome of `TextareaModal::handle_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TextareaAction {
    /// Stay open, redraw.
    Continue,
    /// User pressed Ctrl+Enter or Alt+Enter — here is the captured text.
    Submit(String),
    /// User pressed Esc or Ctrl+C — returns the current buffer (caller
    /// decides whether to discard it).
    Cancel(String),
}

/// Bracketed-paste event text from the terminal. The textarea `handle_paste`
/// method inserts it verbatim (newlines → '\n' in the buffer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PasteEvent(pub(crate) String);

// ─── Model ───────────────────────────────────────────────────────────────────

/// Multi-line textarea modal with word-wrap and vertical scroll.
///
/// The buffer is a flat `String` with `'\n'` for user-typed newlines. The
/// cursor is a byte offset into the buffer (always on a char boundary).
#[derive(Debug, Clone)]
pub(crate) struct TextareaModal {
    pub(crate) title: String,
    pub(crate) prompt: String,
    /// The raw text buffer. `'\n'` = logical line break (user pressed Enter).
    pub(crate) buffer: String,
    /// Byte offset of the cursor inside `buffer`.
    pub(crate) cursor: usize,
    /// Maximum number of visible rows before the area scrolls.
    pub(crate) max_visible_rows: u16,
    /// First visible visual-row index (0-based). Updated on render to keep
    /// the cursor row visible.
    pub(crate) scroll_offset: usize,
}

impl TextareaModal {
    /// Construct a new modal. Defaults to 15 max visible rows.
    pub(crate) fn new(title: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            prompt: prompt.into(),
            buffer: String::new(),
            cursor: 0,
            max_visible_rows: 15,
            scroll_offset: 0,
        }
    }

    /// Builder: override the max visible row count.
    #[allow(dead_code)]
    pub(crate) fn with_max_rows(mut self, rows: u16) -> Self {
        self.max_visible_rows = rows;
        self
    }

    // ─── Key handling ─────────────────────────────────────────────────────────

    /// Process one keystroke. Returns the resulting action.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> TextareaAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        // Ctrl+C / Esc → cancel.
        if ctrl && matches!(key.code, KeyCode::Char('c')) {
            return TextareaAction::Cancel(self.buffer.clone());
        }

        // Ctrl+Enter or Alt+Enter → submit.
        if (ctrl || alt) && matches!(key.code, KeyCode::Enter) {
            return TextareaAction::Submit(std::mem::take(&mut self.buffer));
        }

        // Ctrl+U → clear.
        if ctrl && matches!(key.code, KeyCode::Char('u') | KeyCode::Char('U')) {
            self.buffer.clear();
            self.cursor = 0;
            self.scroll_offset = 0;
            return TextareaAction::Continue;
        }

        match key.code {
            KeyCode::Esc => TextareaAction::Cancel(self.buffer.clone()),

            KeyCode::Enter => {
                // Regular Enter inserts a newline.
                self.buffer.insert(self.cursor, '\n');
                self.cursor += 1;
                TextareaAction::Continue
            }

            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let prev = prev_char_boundary(&self.buffer, self.cursor);
                    self.buffer.replace_range(prev..self.cursor, "");
                    self.cursor = prev;
                }
                TextareaAction::Continue
            }

            KeyCode::Delete => {
                if self.cursor < self.buffer.len() {
                    let next = next_char_boundary(&self.buffer, self.cursor);
                    self.buffer.replace_range(self.cursor..next, "");
                }
                TextareaAction::Continue
            }

            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor = prev_char_boundary(&self.buffer, self.cursor);
                }
                TextareaAction::Continue
            }

            KeyCode::Right => {
                if self.cursor < self.buffer.len() {
                    self.cursor = next_char_boundary(&self.buffer, self.cursor);
                }
                TextareaAction::Continue
            }

            KeyCode::Home => {
                // Jump to start of the current logical line.
                let sol = start_of_logical_line(&self.buffer, self.cursor);
                self.cursor = sol;
                TextareaAction::Continue
            }

            KeyCode::End => {
                // Jump to end of the current logical line.
                let eol = end_of_logical_line(&self.buffer, self.cursor);
                self.cursor = eol;
                TextareaAction::Continue
            }

            KeyCode::Up => {
                // Move cursor up one visual row (respects word-wrap at an
                // estimated inner width of 60 chars — exact width is only
                // known at render time, but 60 is a safe default for
                // movement-feel parity with the rendered view).
                self.cursor = move_cursor_vertical(&self.buffer, self.cursor, -1, 60);
                TextareaAction::Continue
            }

            KeyCode::Down => {
                self.cursor = move_cursor_vertical(&self.buffer, self.cursor, 1, 60);
                TextareaAction::Continue
            }

            KeyCode::Char(ch) => {
                if !ctrl && !alt && !ch.is_control() {
                    self.buffer.insert(self.cursor, ch);
                    self.cursor += ch.len_utf8();
                }
                TextareaAction::Continue
            }

            _ => TextareaAction::Continue,
        }
    }

    /// Process a bracketed-paste event. Inserts the paste text at the
    /// cursor. Normalises `\r\n` and bare `\r` to `\n`.
    pub(crate) fn handle_paste(&mut self, text: &str) {
        let normalised: String = text
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        self.buffer.insert_str(self.cursor, &normalised);
        self.cursor += normalised.len();
    }

    // ─── Stats helpers ────────────────────────────────────────────────────────

    /// Character count (Unicode scalars, not bytes).
    pub(crate) fn char_count(&self) -> usize {
        self.buffer.chars().count()
    }

    /// Approximate word count (whitespace-split).
    pub(crate) fn word_count(&self) -> usize {
        self.buffer.split_whitespace().count()
    }

    // ─── Render ───────────────────────────────────────────────────────────────

    /// Render the textarea as a centered modal overlay.
    ///
    /// `area` is the full terminal area. The modal sizes itself to
    /// `area.width - 8` (capped at 80 cols) and up to
    /// `self.max_visible_rows + 5` rows (border + prompt + footer + stats).
    pub(crate) fn render(&mut self, frame: &mut Frame, area: Rect, accent: Color) {
        let modal_w = area.width.saturating_sub(8).min(80).max(30);
        // inner_w: available text columns (border=2, padding=2 each side).
        let inner_w = modal_w.saturating_sub(4).max(10) as usize;

        // Build the list of visual rows for the current buffer.
        let visual_rows = word_wrap_buffer(&self.buffer, inner_w);

        // Determine which visual row the cursor is on and its column.
        let (cursor_visual_row, cursor_col) =
            cursor_visual_position(&self.buffer, self.cursor, inner_w);

        // Clamp max_visible_rows to what actually fits.
        let max_vis = self.max_visible_rows as usize;
        let vis_rows = max_vis.min(visual_rows.len().max(1));

        // Scroll so the cursor row is visible.
        if cursor_visual_row < self.scroll_offset {
            self.scroll_offset = cursor_visual_row;
        }
        if cursor_visual_row >= self.scroll_offset + vis_rows {
            self.scroll_offset = cursor_visual_row + 1 - vis_rows;
        }

        // Modal height: border(2) + blank(1) + prompt(1) + blank(1) +
        //               vis_rows + blank(1) + stats(1) + hint(1) + blank(1)
        let modal_h = (vis_rows as u16) + 9;
        let modal_h = modal_h.min(area.height.saturating_sub(2));

        if area.width < 14 || area.height < 7 {
            return;
        }

        let modal_x = (area.width.saturating_sub(modal_w)) / 2;
        let modal_y = (area.height.saturating_sub(modal_h)) / 2;
        let modal_area = Rect {
            x: modal_x,
            y: modal_y,
            width: modal_w,
            height: modal_h,
        };

        // Region-scoped Clear — never the full screen (flash anti-pattern #622).
        frame.render_widget(Clear, modal_area);

        let block = Block::default()
            .title(format!(" {} ", self.title))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            )
            .style(Style::default().bg(Color::Black));
        let inner = block.inner(modal_area);
        frame.render_widget(block, modal_area);

        let secondary = super::modal_secondary_color();

        let mut lines: Vec<Line<'static>> = Vec::new();

        // blank + prompt.
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {}", self.prompt),
            Style::default().fg(secondary),
        )));
        lines.push(Line::from(""));

        // Text area rows (the scrolled window into visual_rows).
        let visible_slice_end = (self.scroll_offset + vis_rows).min(visual_rows.len());
        let visible_rows_slice = if visual_rows.is_empty() {
            &[] as &[VisualRow]
        } else {
            &visual_rows[self.scroll_offset..visible_slice_end]
        };

        if visible_rows_slice.is_empty() || self.buffer.is_empty() {
            // Ghost hint: show prompt repeated as placeholder.
            lines.push(Line::from(Span::styled(
                "  <type your description here>".to_string(),
                Style::default().fg(secondary),
            )));
        } else {
            for row in visible_rows_slice {
                let display_text = format!("  {}", row.text);
                lines.push(Line::from(Span::styled(
                    display_text,
                    Style::default().fg(Color::Cyan),
                )));
            }
        }

        // Pad remaining visible rows with blank lines so the layout is stable.
        let filled = visible_rows_slice.len().max(1);
        for _ in filled..vis_rows {
            lines.push(Line::from(""));
        }

        // Stats row.
        lines.push(Line::from(""));
        let stats = format!(
            "  {} words · {} chars",
            self.word_count(),
            self.char_count()
        );
        lines.push(Line::from(Span::styled(
            stats,
            Style::default().fg(secondary),
        )));

        // Hint row.
        lines.push(Line::from(Span::styled(
            "  Enter: newline   Ctrl+Enter: submit   Esc: cancel".to_string(),
            Style::default().fg(secondary),
        )));

        let para = Paragraph::new(Text::from(lines));
        frame.render_widget(para, inner);

        // Place the terminal cursor on the correct visual cell.
        if !self.buffer.is_empty() {
            // Cursor row within the visible window.
            let row_in_window = cursor_visual_row.saturating_sub(self.scroll_offset) as u16;
            // Layout: border(1) + blank(1) + prompt(1) + blank(1) + text rows start.
            let text_area_start_row: u16 = 3; // rows: blank + prompt + blank (0-indexed inside inner)
            let cursor_y = inner.y.saturating_add(text_area_start_row).saturating_add(row_in_window);
            // Column: 2-char padding + col offset.
            let cursor_x = inner.x.saturating_add(2).saturating_add(cursor_col as u16);

            if cursor_x < inner.x + inner.width && cursor_y < inner.y + inner.height {
                frame.set_cursor_position((cursor_x, cursor_y));
            }
        }
    }
}

// ─── Visual-row model ─────────────────────────────────────────────────────────

/// One visual row of rendered text. `byte_start` and `byte_end` are byte
/// offsets into the source buffer (exclusive). `is_hard_break` is `true`
/// when the row ends with a `'\n'` that the user typed.
#[derive(Debug, Clone)]
pub(crate) struct VisualRow {
    /// The display text for this visual row (no trailing newline).
    pub(crate) text: String,
    /// Byte offset in the source buffer where this row starts.
    pub(crate) byte_start: usize,
    /// Byte offset one past the last char of this row's content (before
    /// any trailing `'\n'`).
    pub(crate) byte_end: usize,
    /// True when this is the last wrap-segment of a logical line that ends
    /// with a `'\n'`.
    pub(crate) is_hard_break: bool,
}

/// Decompose the buffer into visual rows by:
/// 1. Splitting on `'\n'` into logical lines.
/// 2. Word-wrapping each logical line to `width` columns.
///
/// Returns an empty `Vec` for an empty buffer (caller handles the ghost
/// hint).
///
/// Byte offsets in each `VisualRow` are accurate positions into `buffer`.
/// Spaces between words that triggered a soft wrap are NOT included in any
/// row's `byte_end`/`byte_start` range — they are considered separator
/// bytes consumed at wrap boundaries.
pub(crate) fn word_wrap_buffer(buffer: &str, width: usize) -> Vec<VisualRow> {
    if buffer.is_empty() {
        return Vec::new();
    }
    let width = width.max(4);
    let mut rows: Vec<VisualRow> = Vec::new();

    // Walk the buffer line by line. We track `line_byte_start` so each
    // logical line's words get accurate buffer-relative byte offsets.
    let mut line_byte_start: usize = 0;

    for raw_line in buffer.split('\n') {
        let line_byte_end = line_byte_start + raw_line.len();
        // Check whether this logical line is followed by a '\n' in the buffer.
        // That is true when the character after line_byte_end is within the buffer.
        let has_newline = line_byte_end < buffer.len();

        // Special case: skip the final empty segment that appears when the
        // buffer ends with '\n' (e.g. "abc\n" → split gives ["abc", ""] and
        // we should NOT emit a visual row for the trailing "").
        // We still need the cursor to be placeable there, but `cursor_visual_position`
        // handles that via its past-the-end fallback.
        if raw_line.is_empty() && !has_newline {
            line_byte_start = line_byte_end + 1;
            continue;
        }

        if raw_line.is_empty() {
            // Mid-buffer empty line (e.g. the line between two newlines "a\n\nb").
            rows.push(VisualRow {
                text: String::new(),
                byte_start: line_byte_start,
                byte_end: line_byte_start, // zero-length content
                is_hard_break: true,
            });
            line_byte_start = line_byte_end + 1;
            continue;
        }

        // Split the logical line into word-wrapped visual segments.
        // We need to track byte offsets within `raw_line`, then add
        // `line_byte_start` to get buffer-relative offsets.
        let segs = wrap_line_with_offsets(raw_line, width);
        let seg_count = segs.len();
        for (seg_idx, (seg_text, seg_rel_start, seg_rel_end)) in segs.into_iter().enumerate() {
            let is_hard_break = has_newline && seg_idx == seg_count - 1;
            rows.push(VisualRow {
                text: seg_text,
                byte_start: line_byte_start + seg_rel_start,
                byte_end: line_byte_start + seg_rel_end,
                is_hard_break,
            });
        }

        line_byte_start = line_byte_end + if has_newline { 1 } else { 0 };
    }
    rows
}

/// Wrap a single logical line (no embedded `'\n'`) into word-wrapped segments,
/// returning `(display_text, rel_byte_start, rel_byte_end)` triples.
/// Byte offsets are relative to the start of `line`.
///
/// Spaces at soft-wrap boundaries are consumed (not included in the display
/// text of either segment). This ensures that the byte offset of the first
/// character of the next visual row correctly points to that character in the
/// original buffer.
fn wrap_line_with_offsets(line: &str, width: usize) -> Vec<(String, usize, usize)> {
    if line.is_empty() {
        return vec![(String::new(), 0, 0)];
    }
    let width = width.max(4);

    // We iterate word-by-word tracking byte positions within `line`.
    let mut segments: Vec<(String, usize, usize)> = Vec::new();
    let mut current_text = String::new();
    let mut current_cols: usize = 0;
    // byte offset of the first char of current_text within `line`.
    let mut current_start: usize = 0;
    // byte offset one past the last char of current_text within `line`.
    let mut current_end: usize = 0;
    // Whether we need to consume the leading space of the next word (i.e.
    // the inter-word space in `line` that comes after the current segment).
    // We track whether we have already started a segment by checking current_text.

    let mut pos: usize = 0; // current byte position in `line`

    // Split the line on ' ' but keep track of where each space is.
    let mut first_word = true;
    let mut words = line.split(' ').peekable();
    while let Some(word) = words.next() {
        let word_byte_len = word.len();
        let word_cols = word.chars().count();
        let has_more = words.peek().is_some();

        // pos is now pointing at the start of `word` in `line` (spaces consumed
        // by prior iterations).
        let word_start = pos;
        let word_end = pos + word_byte_len;

        if first_word {
            // Start the very first segment.
            current_text.push_str(word);
            current_start = word_start;
            current_end = word_end;
            current_cols = word_cols;

            // Handle the case where the first word itself exceeds width.
            if word_cols > width {
                // Hard-split: emit full-width chunks.
                let mut seg_text = String::new();
                let mut seg_cols = 0usize;
                let mut seg_start = word_start;
                let mut seg_pos = word_start;
                for ch in word.chars() {
                    if seg_cols + 1 > width {
                        segments.push((seg_text.clone(), seg_start, seg_pos));
                        seg_text.clear();
                        seg_start = seg_pos;
                        seg_cols = 0;
                    }
                    seg_text.push(ch);
                    seg_pos += ch.len_utf8();
                    seg_cols += 1;
                }
                current_text = seg_text;
                current_start = seg_start;
                current_end = word_end;
                current_cols = seg_cols;
            }
            first_word = false;
        } else {
            // Try to add " word" to the current segment.
            if current_cols + 1 + word_cols <= width {
                // Fits: include the space + word.
                current_text.push(' ');
                current_text.push_str(word);
                current_end = word_end;
                current_cols += 1 + word_cols;
            } else {
                // Doesn't fit: flush current segment.
                segments.push((
                    std::mem::take(&mut current_text),
                    current_start,
                    current_end,
                ));
                // The space before `word` is consumed (it's the wrap-point space).
                // `word` starts at `word_start` in `line`.
                if word_cols <= width {
                    current_text.push_str(word);
                    current_start = word_start;
                    current_end = word_end;
                    current_cols = word_cols;
                } else {
                    // Hard-split the long word.
                    let mut seg_text = String::new();
                    let mut seg_cols = 0usize;
                    let mut seg_start = word_start;
                    let mut seg_pos = word_start;
                    for ch in word.chars() {
                        if seg_cols + 1 > width {
                            segments.push((seg_text.clone(), seg_start, seg_pos));
                            seg_text.clear();
                            seg_start = seg_pos;
                            seg_cols = 0;
                        }
                        seg_text.push(ch);
                        seg_pos += ch.len_utf8();
                        seg_cols += 1;
                    }
                    current_text = seg_text;
                    current_start = seg_start;
                    current_end = word_end;
                    current_cols = seg_cols;
                }
            }
        }

        // Advance past this word + the trailing space (if any).
        pos = word_end;
        if has_more {
            pos += 1; // skip the ' ' separator
        }
    }

    // Flush the final segment.
    if !current_text.is_empty() || segments.is_empty() {
        segments.push((current_text, current_start, current_end));
    }
    segments
}

// ─── Cursor geometry ─────────────────────────────────────────────────────────

/// Given a buffer and cursor byte offset, return `(visual_row_index, col)`.
/// `col` is the 0-based column of the cursor in the visual row.
pub(crate) fn cursor_visual_position(
    buffer: &str,
    cursor: usize,
    inner_w: usize,
) -> (usize, usize) {
    if buffer.is_empty() || inner_w == 0 {
        return (0, 0);
    }
    let rows = word_wrap_buffer(buffer, inner_w);
    if rows.is_empty() {
        return (0, 0);
    }

    // Find the visual row whose byte range contains the cursor.
    // A cursor at byte_end sits on the NEXT row if that row exists (after
    // a soft wrap), but on the current row after a hard break (the cursor
    // is between the last char of the line and the '\n').
    for (row_idx, row) in rows.iter().enumerate() {
        let covers_cursor = if row.is_hard_break || row_idx + 1 == rows.len() {
            // Hard break or last row: cursor at byte_end is on THIS row
            // (position is at end of content on this row, before the \n).
            cursor >= row.byte_start && cursor <= row.byte_end
        } else {
            // Soft wrap: cursor at byte_end belongs to the NEXT row.
            cursor >= row.byte_start && cursor < row.byte_end
        };

        if covers_cursor {
            // Column = char-count of the displayed text up to the cursor.
            let slice_end = cursor.min(row.byte_end);
            let col = if slice_end >= row.byte_start {
                buffer[row.byte_start..slice_end].chars().count()
            } else {
                0
            };
            return (row_idx, col);
        }
    }

    // Cursor is past the end (e.g. buffer ends with \n and cursor is at len).
    (rows.len().saturating_sub(1), 0)
}

/// Move the cursor by `delta` visual rows (±1 for Up/Down). Returns the new
/// byte offset. Uses `approx_inner_w` as the wrap width for movement
/// calculation.
pub(crate) fn move_cursor_vertical(
    buffer: &str,
    cursor: usize,
    delta: i32,
    approx_inner_w: usize,
) -> usize {
    if buffer.is_empty() {
        return 0;
    }
    let rows = word_wrap_buffer(buffer, approx_inner_w);
    if rows.is_empty() {
        return cursor;
    }
    let (cur_row, col) = cursor_visual_position(buffer, cursor, approx_inner_w);
    let target_row = (cur_row as i64 + delta as i64)
        .max(0)
        .min((rows.len() as i64) - 1) as usize;

    if target_row == cur_row {
        return cursor; // already at top/bottom.
    }
    let trow = &rows[target_row];
    // Place the cursor at the same column on the target row, clamped to the row length.
    let target_col = col.min(trow.text.chars().count());
    // Walk `target_col` chars from byte_start.
    let mut byte_pos = trow.byte_start;
    let mut col_counted = 0usize;
    for ch in buffer[trow.byte_start..].chars() {
        if col_counted >= target_col {
            break;
        }
        if ch == '\n' {
            break;
        }
        byte_pos += ch.len_utf8();
        col_counted += 1;
    }
    byte_pos
}

// ─── Char-boundary helpers ───────────────────────────────────────────────────

/// Walk back from `idx` to the previous UTF-8 char boundary.
pub(crate) fn prev_char_boundary(s: &str, idx: usize) -> usize {
    if idx == 0 {
        return 0;
    }
    let mut i = idx - 1;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Walk forward from `idx` to the next UTF-8 char boundary.
pub(crate) fn next_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx + 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Return the byte offset of the start of the logical line containing `cursor`.
fn start_of_logical_line(s: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }
    let before = &s[..cursor.min(s.len())];
    match before.rfind('\n') {
        Some(pos) => pos + 1,
        None => 0,
    }
}

/// Return the byte offset of the end of the logical line containing `cursor`
/// (i.e. the position just before the next `'\n'`, or `s.len()` if none).
fn end_of_logical_line(s: &str, cursor: usize) -> usize {
    let start = cursor.min(s.len());
    match s[start..].find('\n') {
        Some(rel) => start + rel,
        None => s.len(),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }
    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }

    // ─── Buffer model ────────────────────────────────────────────────────────

    #[test]
    fn textarea_insert_chars() {
        let mut m = TextareaModal::new("T", "p");
        m.handle_key(key(KeyCode::Char('h')));
        m.handle_key(key(KeyCode::Char('i')));
        assert_eq!(m.buffer, "hi");
        assert_eq!(m.cursor, 2);
    }

    #[test]
    fn textarea_enter_inserts_newline() {
        let mut m = TextareaModal::new("T", "p");
        m.handle_key(key(KeyCode::Char('a')));
        let action = m.handle_key(key(KeyCode::Enter));
        assert_eq!(action, TextareaAction::Continue);
        assert_eq!(m.buffer, "a\n");
        assert_eq!(m.cursor, 2);
    }

    #[test]
    fn textarea_ctrl_enter_submits() {
        let mut m = TextareaModal::new("T", "p");
        m.handle_key(key(KeyCode::Char('h')));
        m.handle_key(key(KeyCode::Char('i')));
        match m.handle_key(ctrl(KeyCode::Enter)) {
            TextareaAction::Submit(s) => assert_eq!(s, "hi"),
            other => panic!("expected Submit, got {other:?}"),
        }
        // Buffer cleared after submit.
        assert!(m.buffer.is_empty());
    }

    #[test]
    fn textarea_alt_enter_submits() {
        let mut m = TextareaModal::new("T", "p");
        m.handle_key(key(KeyCode::Char('x')));
        match m.handle_key(alt(KeyCode::Enter)) {
            TextareaAction::Submit(s) => assert_eq!(s, "x"),
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn textarea_esc_cancels_with_buffer() {
        let mut m = TextareaModal::new("T", "p");
        m.handle_key(key(KeyCode::Char('a')));
        match m.handle_key(key(KeyCode::Esc)) {
            TextareaAction::Cancel(s) => assert_eq!(s, "a"),
            other => panic!("expected Cancel, got {other:?}"),
        }
        // Buffer NOT cleared on cancel.
        assert_eq!(m.buffer, "a");
    }

    #[test]
    fn textarea_ctrl_c_cancels() {
        let mut m = TextareaModal::new("T", "p");
        m.handle_key(key(KeyCode::Char('z')));
        match m.handle_key(ctrl(KeyCode::Char('c'))) {
            TextareaAction::Cancel(_) => {}
            other => panic!("expected Cancel, got {other:?}"),
        }
    }

    #[test]
    fn textarea_backspace_deletes() {
        let mut m = TextareaModal::new("T", "p");
        for ch in "abc".chars() {
            m.handle_key(key(KeyCode::Char(ch)));
        }
        m.handle_key(key(KeyCode::Backspace));
        assert_eq!(m.buffer, "ab");
        assert_eq!(m.cursor, 2);
    }

    #[test]
    fn textarea_backspace_across_newline() {
        let mut m = TextareaModal::new("T", "p");
        m.handle_key(key(KeyCode::Char('a')));
        m.handle_key(key(KeyCode::Enter)); // "a\n"
        m.handle_key(key(KeyCode::Char('b'))); // "a\nb"
        m.handle_key(key(KeyCode::Backspace)); // deletes 'b' → "a\n"
        assert_eq!(m.buffer, "a\n");
        m.handle_key(key(KeyCode::Backspace)); // deletes '\n' → "a"
        assert_eq!(m.buffer, "a");
    }

    #[test]
    fn textarea_ctrl_u_clears() {
        let mut m = TextareaModal::new("T", "p");
        m.handle_key(key(KeyCode::Char('x')));
        m.handle_key(key(KeyCode::Enter));
        m.handle_key(key(KeyCode::Char('y')));
        m.handle_key(ctrl(KeyCode::Char('u')));
        assert!(m.buffer.is_empty());
        assert_eq!(m.cursor, 0);
        assert_eq!(m.scroll_offset, 0);
    }

    #[test]
    fn textarea_home_end_navigation() {
        let mut m = TextareaModal::new("T", "p");
        for ch in "hello world".chars() {
            m.handle_key(key(KeyCode::Char(ch)));
        }
        m.handle_key(key(KeyCode::Home));
        assert_eq!(m.cursor, 0);
        m.handle_key(key(KeyCode::End));
        assert_eq!(m.cursor, 11);
    }

    #[test]
    fn textarea_home_end_on_second_line() {
        let mut m = TextareaModal::new("T", "p");
        m.handle_key(key(KeyCode::Char('a')));
        m.handle_key(key(KeyCode::Enter));
        m.handle_key(key(KeyCode::Char('b')));
        m.handle_key(key(KeyCode::Char('c')));
        // Cursor at position 4 ("a\nbc", cursor after 'c').
        assert_eq!(m.cursor, 4);
        m.handle_key(key(KeyCode::Home));
        assert_eq!(m.cursor, 2); // start of "bc" line
        m.handle_key(key(KeyCode::End));
        assert_eq!(m.cursor, 4); // end of "bc" line
    }

    #[test]
    fn textarea_paste_inserts_text() {
        let mut m = TextareaModal::new("T", "p");
        m.handle_paste("hello\nworld");
        assert_eq!(m.buffer, "hello\nworld");
        assert_eq!(m.cursor, 11);
    }

    #[test]
    fn textarea_paste_normalises_crlf() {
        let mut m = TextareaModal::new("T", "p");
        m.handle_paste("line1\r\nline2\rline3");
        assert_eq!(m.buffer, "line1\nline2\nline3");
    }

    #[test]
    fn textarea_word_count() {
        let mut m = TextareaModal::new("T", "p");
        // "hello world\nfoo bar baz" = 5+1+5+1+3+1+3+1+3 = 23 chars, 5 words
        m.handle_paste("hello world\nfoo bar baz");
        assert_eq!(m.word_count(), 5);
        assert_eq!(m.char_count(), 23);
    }

    // ─── Large buffer (2000 chars) ────────────────────────────────────────────

    /// Every character in a 2000-char buffer must be addressable via the
    /// model (cursor can reach every byte offset). We verify by navigating
    /// Left from the end to the start and checking the final cursor position.
    #[test]
    fn textarea_2000_char_buffer_fully_addressable() {
        let long_text: String = "word ".repeat(400); // 2000 chars
        assert_eq!(long_text.len(), 2000);

        let mut m = TextareaModal::new("T", "p");
        m.handle_paste(&long_text);
        assert_eq!(m.cursor, 2000);

        // Walk backwards: every position from 2000 down to 0 must be reachable.
        for expected_cursor in (0..2000usize).rev() {
            m.handle_key(key(KeyCode::Left));
            assert_eq!(
                m.cursor, expected_cursor,
                "cursor should be {expected_cursor} after {} Left presses",
                2000 - expected_cursor
            );
        }
        // Boundary: Left at position 0 is a no-op.
        m.handle_key(key(KeyCode::Left));
        assert_eq!(m.cursor, 0);
    }

    /// Ctrl+Enter on a 2000-char buffer must submit the full text.
    #[test]
    fn textarea_2000_char_ctrl_enter_submits_full_buffer() {
        let long_text: String = "word ".repeat(400);
        let mut m = TextareaModal::new("T", "p");
        m.handle_paste(&long_text);
        match m.handle_key(ctrl(KeyCode::Enter)) {
            TextareaAction::Submit(s) => {
                assert_eq!(
                    s.len(),
                    2000,
                    "submitted text must be the full 2000-char buffer"
                );
                assert_eq!(s, long_text);
            }
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    // ─── word_wrap_buffer ────────────────────────────────────────────────────

    #[test]
    fn wrap_buffer_empty() {
        assert!(word_wrap_buffer("", 60).is_empty());
    }

    #[test]
    fn wrap_buffer_single_short_line() {
        let rows = word_wrap_buffer("hello", 60);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "hello");
        assert_eq!(rows[0].byte_start, 0);
        assert_eq!(rows[0].byte_end, 5);
        assert!(!rows[0].is_hard_break);
    }

    #[test]
    fn wrap_buffer_two_logical_lines() {
        let rows = word_wrap_buffer("foo\nbar", 60);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "foo");
        assert!(rows[0].is_hard_break);
        assert_eq!(rows[1].text, "bar");
        assert!(!rows[1].is_hard_break);
    }

    #[test]
    fn wrap_buffer_soft_wrap() {
        // "hello world" at width 7 → ["hello", "world"]
        let rows = word_wrap_buffer("hello world", 7);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "hello");
        assert_eq!(rows[1].text, "world");
        assert!(!rows[0].is_hard_break);
        assert!(!rows[1].is_hard_break);
    }

    #[test]
    fn wrap_buffer_byte_offsets_contiguous() {
        // Verify that byte_start of each row correctly points to the first
        // char of that visual row in the buffer, and byte_end points past
        // the last char (before any inter-word space or newline).
        //
        // "aaa bbb ccc ddd" at width 4: wraps as ["aaa", "bbb", "ccc", "ddd"].
        // Byte ranges: [0..3], [4..7], [8..11], [12..15].
        let text = "aaa bbb ccc ddd";
        let rows = word_wrap_buffer(text, 4);
        assert!(rows.len() >= 2, "should produce multiple rows at width 4");
        // Every row's byte range must correspond to valid chars in the buffer.
        for row in &rows {
            assert!(
                row.byte_start <= row.byte_end,
                "byte_start must not exceed byte_end"
            );
            assert!(
                row.byte_end <= text.len(),
                "byte_end must not exceed buffer length"
            );
            // The text at the row's byte range must equal the row's display text.
            assert_eq!(
                &text[row.byte_start..row.byte_end],
                &row.text,
                "row text must match buffer slice"
            );
        }
    }

    #[test]
    fn wrap_buffer_trailing_newline() {
        let rows = word_wrap_buffer("abc\n", 60);
        assert_eq!(rows.len(), 1, "trailing newline should not produce an extra blank row from the wrapper itself");
        assert_eq!(rows[0].text, "abc");
        assert!(rows[0].is_hard_break);
    }

    // ─── cursor_visual_position ───────────────────────────────────────────────

    #[test]
    fn cursor_visual_position_start() {
        let buf = "hello world";
        assert_eq!(cursor_visual_position(buf, 0, 60), (0, 0));
    }

    #[test]
    fn cursor_visual_position_end_of_line() {
        let buf = "hello";
        assert_eq!(cursor_visual_position(buf, 5, 60), (0, 5));
    }

    #[test]
    fn cursor_visual_position_second_line() {
        let buf = "foo\nbar";
        // Cursor at position 4 = start of "bar".
        assert_eq!(cursor_visual_position(buf, 4, 60), (1, 0));
        // Cursor at position 7 = end of "bar".
        assert_eq!(cursor_visual_position(buf, 7, 60), (1, 3));
    }

    #[test]
    fn cursor_visual_position_wrapped_line() {
        // "hello world" wrapped at width 7 → row 0: "hello" (bytes 0..5), row 1: "world" (bytes 6..11).
        let buf = "hello world";
        let pos = cursor_visual_position(buf, 6, 7); // cursor at 'w' of "world"
        assert_eq!(pos, (1, 0), "cursor at start of 'world' should be on row 1 col 0");
    }

    // ─── Render smoke test ────────────────────────────────────────────────────

    #[test]
    fn textarea_render_at_60_width_with_2000_char_input_does_not_panic() {
        let long_text: String = "word ".repeat(400);
        let mut m = TextareaModal::new("AI prompt", "Describe the MCP server");
        m.handle_paste(&long_text);

        let backend = TestBackend::new(60, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            m.render(f, f.area(), Color::Cyan);
        })
        .unwrap();

        // The buffer must still hold all 2000 chars after render.
        assert_eq!(m.buffer.len(), 2000);
    }

    #[test]
    fn textarea_render_empty_shows_ghost_hint() {
        let mut m = TextareaModal::new("Title", "enter text");
        let backend = TestBackend::new(80, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            m.render(f, f.area(), Color::Cyan);
        })
        .unwrap();

        let buf = term.backend().buffer();
        let dump: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            dump.contains("type your description here"),
            "ghost hint missing from empty render: {dump:?}"
        );
    }

    #[test]
    fn textarea_render_small_terminal_does_not_panic() {
        let mut m = TextareaModal::new("X", "Y");
        let backend = TestBackend::new(10, 5);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            m.render(f, f.area(), Color::Cyan);
        })
        .unwrap();
    }
}
