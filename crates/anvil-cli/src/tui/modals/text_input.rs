//! `TextInputModal` — single-line text input overlay (task #642).
//!
//! The first-run wizard (v2.2.17 finisher) needs an inline single-line
//! input prompt that shares the same alt-screen session as the rest of
//! the modal queue.  Use cases:
//!
//! - "Ollama URL" with a default of `http://localhost:11434`
//! - "Profile name" with a default of `default`
//!
//! The modal mirrors the look-and-feel of `PasswordModal` but renders
//! the buffer in cleartext.  When the buffer is empty the configured
//! `default` is rendered as a ghost hint and Enter takes that value;
//! Esc also takes the default (or empty if no default is set).
//!
//! ## Keys
//!
//! - printable chars         insert at cursor
//! - Backspace               delete the char before the cursor
//! - Left / Right arrow      move the cursor
//! - Home / End              jump to start / end
//! - Enter                   submit (`Submit(String)`); takes default when empty
//! - Esc                     cancel (`Cancel(String)`); also returns the default
//! - Ctrl+U                  clear the buffer
//! - Ctrl+C                  cancel (same as Esc)
//!
//! ## Why a separate modal type
//!
//! `PasswordModal` masks the buffer and has no notion of a "default"
//! placeholder.  `WizardChoiceModal` is numbered-list only.  Neither
//! covers free-text-with-default — hence this third modal type.  It
//! plugs into `ModalQueue` via the new `QueuedModal::TextInput` /
//! `ModalAnswer::TextInput` variants.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

/// Outcome of `TextInputModal::handle_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TextInputAction {
    /// Stay open, redraw.
    Continue,
    /// User pressed Enter — here is the captured value (default applied
    /// when the buffer was empty).
    Submit(String),
    /// User pressed Esc — returns the default (or empty if none).
    Cancel(String),
}

/// A single-line text-input modal with an optional default value.
#[derive(Debug, Clone)]
pub(crate) struct TextInputModal {
    pub(crate) title: String,
    pub(crate) prompt: String,
    pub(crate) default: String,
    pub(crate) placeholder: Option<String>,
    pub(crate) buffer: String,
    pub(crate) cursor: usize,
}

impl TextInputModal {
    /// Construct a new modal.  Defaults to an empty buffer + empty
    /// default + no placeholder.
    pub(crate) fn new(title: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            prompt: prompt.into(),
            default: String::new(),
            placeholder: None,
            buffer: String::new(),
            cursor: 0,
        }
    }

    /// Builder: set the default value applied on empty-Enter / Esc.
    pub(crate) fn with_default(mut self, default: impl Into<String>) -> Self {
        self.default = default.into();
        self
    }

    /// Builder: set the placeholder hint shown when the buffer is empty.
    /// When unset the default value is shown as the ghost hint instead.
    #[allow(dead_code)]
    pub(crate) fn with_placeholder(mut self, ph: impl Into<String>) -> Self {
        self.placeholder = Some(ph.into());
        self
    }

    /// Process one keystroke and return the resulting action.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> TextInputAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Ctrl+C cancels (matches OAuth/password modals).
        if ctrl && matches!(key.code, KeyCode::Char('c')) {
            return TextInputAction::Cancel(self.default.clone());
        }
        // Ctrl+U clears the buffer + resets the cursor.
        if ctrl && matches!(key.code, KeyCode::Char('u') | KeyCode::Char('U')) {
            self.buffer.clear();
            self.cursor = 0;
            return TextInputAction::Continue;
        }

        match key.code {
            KeyCode::Esc => TextInputAction::Cancel(self.default.clone()),
            KeyCode::Enter => {
                if self.buffer.is_empty() {
                    TextInputAction::Submit(self.default.clone())
                } else {
                    // Take ownership so the buffer is empty after submit.
                    let value = std::mem::take(&mut self.buffer);
                    self.cursor = 0;
                    TextInputAction::Submit(value)
                }
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    // Walk back one UTF-8 char and remove it.
                    let prev = prev_char_boundary(&self.buffer, self.cursor);
                    self.buffer.replace_range(prev..self.cursor, "");
                    self.cursor = prev;
                }
                TextInputAction::Continue
            }
            KeyCode::Delete => {
                if self.cursor < self.buffer.len() {
                    let next = next_char_boundary(&self.buffer, self.cursor);
                    self.buffer.replace_range(self.cursor..next, "");
                }
                TextInputAction::Continue
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor = prev_char_boundary(&self.buffer, self.cursor);
                }
                TextInputAction::Continue
            }
            KeyCode::Right => {
                if self.cursor < self.buffer.len() {
                    self.cursor = next_char_boundary(&self.buffer, self.cursor);
                }
                TextInputAction::Continue
            }
            KeyCode::Home => {
                self.cursor = 0;
                TextInputAction::Continue
            }
            KeyCode::End => {
                self.cursor = self.buffer.len();
                TextInputAction::Continue
            }
            KeyCode::Char(ch) => {
                if !ctrl && !ch.is_control() {
                    self.buffer.insert(self.cursor, ch);
                    self.cursor += ch.len_utf8();
                }
                TextInputAction::Continue
            }
            _ => TextInputAction::Continue,
        }
    }

    /// Render the modal as a centered overlay.
    pub(crate) fn render(
        &self,
        frame: &mut Frame,
        area: Rect,
        buffer: &str,
        cursor: usize,
        accent: Color,
    ) {
        // Modal sizing — matches PasswordModal's footprint.
        let modal_w = area.width.saturating_sub(8).min(70).max(30);
        let modal_h: u16 = 9;
        if area.width < 12 || area.height < modal_h {
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

        // Region-scoped Clear (not full screen) — flash anti-pattern #622.
        frame.render_widget(Clear, modal_area);

        let block = Block::default()
            .title(format!(" {} ", self.title))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(accent).add_modifier(Modifier::BOLD))
            .style(Style::default().bg(Color::Black));
        let inner = block.inner(modal_area);
        frame.render_widget(block, modal_area);

        // Display string: the live buffer, or the ghost-default/placeholder
        // when empty.  Cursor position is in byte offset; for display we
        // render the raw buffer and let the caller's cursor placement
        // (handled below in the cursor() call) draw the blinking caret.
        let is_empty = buffer.is_empty();
        let ghost: String = if is_empty {
            self.placeholder
                .clone()
                .unwrap_or_else(|| self.default.clone())
        } else {
            String::new()
        };

        let display_line: Line<'static> = if is_empty {
            if ghost.is_empty() {
                Line::from(Span::styled(
                    "  <type a value, Enter to submit>".to_string(),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ))
            } else {
                Line::from(Span::styled(
                    format!("  {ghost}"),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ))
            }
        } else {
            Line::from(Span::styled(
                format!("  {buffer}"),
                Style::default().fg(Color::Cyan),
            ))
        };

        let lines: Vec<Line<'static>> = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {}", self.prompt),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            display_line,
            Line::from(""),
            Line::from(Span::styled(
                if self.default.is_empty() {
                    "  Enter: submit   Esc: cancel   Ctrl+U: clear".to_string()
                } else {
                    format!(
                        "  Enter: submit (default: {})   Esc: cancel   Ctrl+U: clear",
                        self.default
                    )
                },
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )),
        ];

        let para = Paragraph::new(Text::from(lines));
        frame.render_widget(para, inner);

        // Position the cursor on the input line when the user has
        // actually typed something.  Ratatui paints the caret on the
        // exact (x, y) cell — `cursor` is a byte offset, which is also
        // the column offset for ASCII; for multibyte chars we count
        // grapheme width via char count to keep parity with the visible
        // glyphs.
        if !is_empty {
            // Input is rendered on inner row 3 (after blank + prompt + blank).
            let row: u16 = 3;
            // 2-col padding before the buffer in the rendered line.
            let col_offset: u16 = 2;
            let visible_cols = buffer[..cursor.min(buffer.len())].chars().count() as u16;
            let x = inner.x.saturating_add(col_offset).saturating_add(visible_cols);
            let y = inner.y.saturating_add(row);
            if x < inner.x + inner.width && y < inner.y + inner.height {
                frame.set_cursor_position((x, y));
            }
        }
    }
}

/// Walk back from `idx` (a byte offset into `s`) to the previous UTF-8
/// char boundary.
fn prev_char_boundary(s: &str, idx: usize) -> usize {
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
fn next_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx + 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn text_input_modal_renders_default_as_ghost() {
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let modal = TextInputModal::new("Ollama URL", "Endpoint")
            .with_default("http://localhost:11434");
        term.draw(|f| {
            modal.render(f, f.area(), &modal.buffer, modal.cursor, Color::Cyan);
        })
        .unwrap();

        // Inspect the frame buffer: the default value must appear as ghost.
        let buf = term.backend().buffer();
        let mut found_ghost = false;
        for y in 0..buf.area.height {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            if row.contains("http://localhost:11434") {
                found_ghost = true;
                break;
            }
        }
        assert!(
            found_ghost,
            "default value must render as ghost when buffer is empty"
        );
    }

    #[test]
    fn text_input_modal_enter_on_empty_takes_default() {
        let mut modal = TextInputModal::new("URL", "endpoint")
            .with_default("http://localhost:11434");
        match modal.handle_key(key(KeyCode::Enter)) {
            TextInputAction::Submit(value) => {
                assert_eq!(value, "http://localhost:11434");
            }
            other => panic!("expected Submit(default), got {other:?}"),
        }
    }

    #[test]
    fn text_input_modal_esc_takes_default() {
        let mut modal = TextInputModal::new("URL", "endpoint")
            .with_default("http://localhost:11434");
        match modal.handle_key(key(KeyCode::Esc)) {
            TextInputAction::Cancel(value) => {
                assert_eq!(value, "http://localhost:11434");
            }
            other => panic!("expected Cancel(default), got {other:?}"),
        }
    }

    #[test]
    fn text_input_modal_buffer_handles_backspace() {
        let mut modal = TextInputModal::new("Name", "profile");
        modal.handle_key(key(KeyCode::Char('h')));
        modal.handle_key(key(KeyCode::Char('e')));
        modal.handle_key(key(KeyCode::Char('y')));
        assert_eq!(modal.buffer, "hey");
        assert_eq!(modal.cursor, 3);
        modal.handle_key(key(KeyCode::Backspace));
        assert_eq!(modal.buffer, "he");
        assert_eq!(modal.cursor, 2);
        modal.handle_key(key(KeyCode::Backspace));
        modal.handle_key(key(KeyCode::Backspace));
        assert!(modal.buffer.is_empty());
        assert_eq!(modal.cursor, 0);
        // Backspace on empty buffer is a no-op (no underflow).
        modal.handle_key(key(KeyCode::Backspace));
        assert!(modal.buffer.is_empty());
        assert_eq!(modal.cursor, 0);
    }

    #[test]
    fn text_input_modal_buffer_handles_cursor_arrows() {
        let mut modal = TextInputModal::new("Name", "profile");
        modal.handle_key(key(KeyCode::Char('a')));
        modal.handle_key(key(KeyCode::Char('b')));
        modal.handle_key(key(KeyCode::Char('c')));
        assert_eq!(modal.cursor, 3);
        modal.handle_key(key(KeyCode::Left));
        assert_eq!(modal.cursor, 2);
        modal.handle_key(key(KeyCode::Left));
        assert_eq!(modal.cursor, 1);
        modal.handle_key(key(KeyCode::Home));
        assert_eq!(modal.cursor, 0);
        // Insert at start.
        modal.handle_key(key(KeyCode::Char('X')));
        assert_eq!(modal.buffer, "Xabc");
        assert_eq!(modal.cursor, 1);
        modal.handle_key(key(KeyCode::End));
        assert_eq!(modal.cursor, 4);
        // Right at end is a no-op (no overflow).
        modal.handle_key(key(KeyCode::Right));
        assert_eq!(modal.cursor, 4);
        // Left from end walks back.
        modal.handle_key(key(KeyCode::Left));
        assert_eq!(modal.cursor, 3);
    }

    #[test]
    fn text_input_modal_enter_on_filled_submits_buffer() {
        let mut modal = TextInputModal::new("Name", "profile").with_default("default");
        modal.handle_key(key(KeyCode::Char('p')));
        modal.handle_key(key(KeyCode::Char('r')));
        modal.handle_key(key(KeyCode::Char('o')));
        match modal.handle_key(key(KeyCode::Enter)) {
            TextInputAction::Submit(value) => {
                assert_eq!(value, "pro", "filled buffer must override default");
            }
            other => panic!("expected Submit(buffer), got {other:?}"),
        }
    }

    #[test]
    fn text_input_modal_ctrl_u_clears() {
        let mut modal = TextInputModal::new("X", "Y");
        modal.handle_key(key(KeyCode::Char('a')));
        modal.handle_key(key(KeyCode::Char('b')));
        modal.handle_key(ctrl(KeyCode::Char('u')));
        assert!(modal.buffer.is_empty());
        assert_eq!(modal.cursor, 0);
    }

    #[test]
    fn text_input_modal_ctrl_c_cancels_with_default() {
        let mut modal = TextInputModal::new("X", "Y").with_default("fallback");
        match modal.handle_key(ctrl(KeyCode::Char('c'))) {
            TextInputAction::Cancel(value) => assert_eq!(value, "fallback"),
            other => panic!("expected Cancel(default), got {other:?}"),
        }
    }

    #[test]
    fn text_input_modal_delete_removes_char_at_cursor() {
        let mut modal = TextInputModal::new("X", "Y");
        modal.handle_key(key(KeyCode::Char('a')));
        modal.handle_key(key(KeyCode::Char('b')));
        modal.handle_key(key(KeyCode::Char('c')));
        modal.handle_key(key(KeyCode::Home));
        modal.handle_key(key(KeyCode::Delete));
        assert_eq!(modal.buffer, "bc");
        assert_eq!(modal.cursor, 0);
    }

    #[test]
    fn text_input_modal_render_small_terminal_does_not_panic() {
        let backend = TestBackend::new(10, 5);
        let mut term = Terminal::new(backend).unwrap();
        let modal = TextInputModal::new("X", "Y").with_default("d");
        term.draw(|f| {
            modal.render(f, f.area(), &modal.buffer, modal.cursor, Color::Cyan);
        })
        .unwrap();
    }
}
