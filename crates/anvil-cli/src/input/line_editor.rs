//! Line editing, cursor movement, and vim-mode state machine.

use std::borrow::Cow;
use std::io::{self, Write};

use crossterm::cursor::{MoveToColumn, MoveUp};
use crossterm::queue;
use crossterm::terminal::{Clear, ClearType};

// ─── Editor mode ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EditorMode {
    Plain,
    Insert,
    Normal,
    Visual,
    Command,
}

impl EditorMode {
    pub(super) fn indicator(self, vim_enabled: bool) -> Option<&'static str> {
        if !vim_enabled {
            return None;
        }

        Some(match self {
            Self::Plain => "PLAIN",
            Self::Insert => "INSERT",
            Self::Normal => "NORMAL",
            Self::Visual => "VISUAL",
            Self::Command => "COMMAND",
        })
    }
}

// ─── Yank buffer ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct YankBuffer {
    pub(super) text: String,
    pub(super) linewise: bool,
}

// ─── Edit session ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EditSession {
    pub(super) text: String,
    pub(super) cursor: usize,
    pub(super) mode: EditorMode,
    pub(super) pending_operator: Option<char>,
    pub(super) visual_anchor: Option<usize>,
    pub(super) command_buffer: String,
    pub(super) command_cursor: usize,
    pub(super) history_index: Option<usize>,
    pub(super) history_backup: Option<String>,
    pub(super) rendered_cursor_row: usize,
    pub(super) rendered_lines: usize,
}

impl EditSession {
    pub(super) fn new(vim_enabled: bool) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            mode: if vim_enabled {
                EditorMode::Insert
            } else {
                EditorMode::Plain
            },
            pending_operator: None,
            visual_anchor: None,
            command_buffer: String::new(),
            command_cursor: 0,
            history_index: None,
            history_backup: None,
            rendered_cursor_row: 0,
            rendered_lines: 1,
        }
    }

    pub(super) fn active_text(&self) -> &str {
        if self.mode == EditorMode::Command {
            &self.command_buffer
        } else {
            &self.text
        }
    }

    pub(super) fn current_len(&self) -> usize {
        self.active_text().len()
    }

    pub(super) fn has_input(&self) -> bool {
        !self.active_text().is_empty()
    }

    pub(super) fn current_line(&self) -> String {
        self.active_text().to_string()
    }

    pub(super) fn set_text_from_history(&mut self, entry: String) {
        self.text = entry;
        self.cursor = self.text.len();
        self.pending_operator = None;
        self.visual_anchor = None;
        if self.mode != EditorMode::Plain && self.mode != EditorMode::Insert {
            self.mode = EditorMode::Normal;
        }
    }

    pub(super) fn enter_insert_mode(&mut self) {
        self.mode = EditorMode::Insert;
        self.pending_operator = None;
        self.visual_anchor = None;
    }

    pub(super) fn enter_normal_mode(&mut self) {
        self.mode = EditorMode::Normal;
        self.pending_operator = None;
        self.visual_anchor = None;
    }

    pub(super) fn enter_visual_mode(&mut self) {
        self.mode = EditorMode::Visual;
        self.pending_operator = None;
        self.visual_anchor = Some(self.cursor);
    }

    pub(super) fn enter_command_mode(&mut self) {
        self.mode = EditorMode::Command;
        self.pending_operator = None;
        self.visual_anchor = None;
        self.command_buffer.clear();
        self.command_buffer.push(':');
        self.command_cursor = self.command_buffer.len();
    }

    pub(super) fn exit_command_mode(&mut self) {
        self.command_buffer.clear();
        self.command_cursor = 0;
        self.enter_normal_mode();
    }

    pub(super) fn visible_buffer(&self) -> Cow<'_, str> {
        if self.mode != EditorMode::Visual {
            return Cow::Borrowed(self.active_text());
        }

        let Some(anchor) = self.visual_anchor else {
            return Cow::Borrowed(self.active_text());
        };
        let Some((start, end)) = selection_bounds(&self.text, anchor, self.cursor) else {
            return Cow::Borrowed(self.active_text());
        };

        Cow::Owned(render_selected_text(&self.text, start, end))
    }

    pub(super) fn prompt<'a>(&self, base_prompt: &'a str, vim_enabled: bool) -> Cow<'a, str> {
        match self.mode.indicator(vim_enabled) {
            Some(mode) => Cow::Owned(format!("[{mode}] {base_prompt}")),
            None => Cow::Borrowed(base_prompt),
        }
    }

    pub(super) fn clear_render(&self, out: &mut impl Write) -> io::Result<()> {
        if self.rendered_cursor_row > 0 {
            queue!(out, MoveUp(to_u16(self.rendered_cursor_row)?))?;
        }
        queue!(out, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
        out.flush()
    }

    pub(super) fn render(
        &mut self,
        out: &mut impl Write,
        base_prompt: &str,
        vim_enabled: bool,
    ) -> io::Result<()> {
        self.clear_render(out)?;

        let prompt = self.prompt(base_prompt, vim_enabled);
        let buffer = self.visible_buffer();
        write!(out, "{prompt}{buffer}")?;

        let (cursor_row, cursor_col, total_lines) = self.cursor_layout(prompt.as_ref());
        let rows_to_move_up = total_lines.saturating_sub(cursor_row + 1);
        if rows_to_move_up > 0 {
            queue!(out, MoveUp(to_u16(rows_to_move_up)?))?;
        }
        queue!(out, MoveToColumn(to_u16(cursor_col)?))?;
        out.flush()?;

        self.rendered_cursor_row = cursor_row;
        self.rendered_lines = total_lines;
        Ok(())
    }

    pub(super) fn finalize_render(
        &self,
        out: &mut impl Write,
        base_prompt: &str,
        vim_enabled: bool,
    ) -> io::Result<()> {
        self.clear_render(out)?;
        let prompt = self.prompt(base_prompt, vim_enabled);
        let buffer = self.visible_buffer();
        write!(out, "{prompt}{buffer}")?;
        writeln!(out)
    }

    pub(super) fn cursor_layout(&self, prompt: &str) -> (usize, usize, usize) {
        let active_text = self.active_text();
        let cursor = if self.mode == EditorMode::Command {
            self.command_cursor
        } else {
            self.cursor
        };

        let cursor_prefix = &active_text[..cursor];
        let cursor_row = cursor_prefix.bytes().filter(|byte| *byte == b'\n').count();
        let cursor_col = match cursor_prefix.rsplit_once('\n') {
            Some((_, suffix)) => suffix.chars().count(),
            None => prompt.chars().count() + cursor_prefix.chars().count(),
        };
        let total_lines = active_text.bytes().filter(|byte| *byte == b'\n').count() + 1;
        (cursor_row, cursor_col, total_lines)
    }
}

// ─── Cursor / text manipulation helpers ──────────────────────────────────────

pub(super) fn previous_boundary(text: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }

    text[..cursor]
        .char_indices()
        .next_back()
        .map_or(0, |(index, _)| index)
}

pub(super) fn previous_command_boundary(text: &str, cursor: usize) -> usize {
    previous_boundary(text, cursor).max(1)
}

pub(super) fn next_boundary(text: &str, cursor: usize) -> usize {
    if cursor >= text.len() {
        return text.len();
    }

    text[cursor..]
        .chars()
        .next()
        .map_or(text.len(), |ch| cursor + ch.len_utf8())
}

pub(super) fn remove_previous_char(text: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }

    let start = previous_boundary(text, *cursor);
    text.drain(start..*cursor);
    *cursor = start;
}

pub(super) fn line_start(text: &str, cursor: usize) -> usize {
    text[..cursor].rfind('\n').map_or(0, |index| index + 1)
}

pub(super) fn line_end(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .find('\n')
        .map_or(text.len(), |index| cursor + index)
}

pub(super) fn move_vertical(text: &str, cursor: usize, delta: isize) -> usize {
    let starts = line_starts(text);
    let current_row = text[..cursor].bytes().filter(|byte| *byte == b'\n').count();
    let current_start = starts[current_row];
    let current_col = text[current_start..cursor].chars().count();

    let max_row = starts.len().saturating_sub(1) as isize;
    let target_row = (current_row as isize + delta).clamp(0, max_row) as usize;
    if target_row == current_row {
        return cursor;
    }

    let target_start = starts[target_row];
    let target_end = if target_row + 1 < starts.len() {
        starts[target_row + 1] - 1
    } else {
        text.len()
    };
    byte_index_for_char_column(&text[target_start..target_end], current_col) + target_start
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, ch) in text.char_indices() {
        if ch == '\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn byte_index_for_char_column(text: &str, column: usize) -> usize {
    let mut current = 0;
    for (index, _) in text.char_indices() {
        if current == column {
            return index;
        }
        current += 1;
    }
    text.len()
}

pub(super) fn current_line_delete_range(text: &str, cursor: usize) -> (usize, usize, usize) {
    let line_start_idx = line_start(text, cursor);
    let line_end_core = line_end(text, cursor);
    let line_end_idx = if line_end_core < text.len() {
        line_end_core + 1
    } else {
        line_end_core
    };
    let delete_start_idx = if line_end_idx == text.len() && line_start_idx > 0 {
        line_start_idx - 1
    } else {
        line_start_idx
    };
    (line_start_idx, line_end_idx, delete_start_idx)
}

pub(super) fn selection_bounds(text: &str, anchor: usize, cursor: usize) -> Option<(usize, usize)> {
    if text.is_empty() {
        return None;
    }

    if cursor >= anchor {
        let end = next_boundary(text, cursor);
        Some((anchor.min(text.len()), end.min(text.len())))
    } else {
        let end = next_boundary(text, anchor);
        Some((cursor.min(text.len()), end.min(text.len())))
    }
}

pub(super) fn render_selected_text(text: &str, start: usize, end: usize) -> String {
    let mut rendered = String::new();
    let mut in_selection = false;

    for (index, ch) in text.char_indices() {
        if !in_selection && index == start {
            rendered.push_str("\x1b[7m");
            in_selection = true;
        }
        if in_selection && index == end {
            rendered.push_str("\x1b[0m");
            in_selection = false;
        }
        rendered.push(ch);
    }

    if in_selection {
        rendered.push_str("\x1b[0m");
    }

    rendered
}

pub(super) fn to_u16(value: usize) -> io::Result<u16> {
    u16::try_from(value).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal position overflowed u16",
        )
    })
}
