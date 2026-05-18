//! `PasswordModal` — masked single-line input overlay (task #627).
//!
//! Replaces `eprint!("Master password: "); rpassword::read_password()`
//! which competed with ratatui for stdin and painted into the
//! alt-screen back-buffer.  Renders a centered popup with a single
//! input field whose characters are displayed as `*` glyphs.
//!
//! ## Keys
//! - printable chars   append to buffer
//! - `Backspace`       pop last char
//! - `Enter`           submit (returns `Submit(String)`)
//! - `Esc`             cancel  (returns `Cancel`)
//! - `Ctrl+U`          clear buffer
//!
//! ## Security
//! The buffer is never echoed back — the render always emits `*`
//! glyphs, one per UTF-8 character.  The buffer length is implicitly
//! visible (as the count of mask characters), which matches the
//! standard behavior of macOS Keychain / Linux PAM prompts.
//!
//! ## Retry semantics
//! The modal itself has no notion of attempt counts — that policy is
//! enforced by the host.  When a submission fails (e.g. wrong vault
//! password), the host calls [`PasswordModal::set_error`] with the
//! error string and the modal stays open; the buffer is cleared and
//! the input regains focus on the next redraw.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

/// The outcome of `PasswordModal::handle_key`.
#[derive(Debug, Clone)]
pub(crate) enum PasswordAction {
    /// Modal stays open; redraw and continue.
    Continue,
    /// User pressed Enter; here is the captured password.
    Submit(String),
    /// User pressed Esc.
    Cancel,
}

/// A masked single-line input modal.
#[derive(Debug, Clone)]
pub(crate) struct PasswordModal {
    pub(crate) title: String,
    pub(crate) prompt: String,
    pub(crate) buffer: String,
    pub(crate) error: Option<String>,
    /// How many submission attempts have been recorded so far.  The
    /// host increments this on a failed attempt before re-opening the
    /// modal; useful when rendering "Attempt N/3" hints.
    pub(crate) attempts: u32,
}

impl PasswordModal {
    /// Create a new modal with an empty buffer and no error.
    pub(crate) fn new(title: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            prompt: prompt.into(),
            buffer: String::new(),
            error: None,
            attempts: 0,
        }
    }

    /// Set an error string (e.g. "Wrong password — try again") and
    /// clear the buffer so the user starts fresh.  Caller is expected
    /// to leave the modal open after this so the user can retry.
    pub(crate) fn set_error(&mut self, msg: impl Into<String>) {
        self.error = Some(msg.into());
        self.buffer.clear();
        self.attempts = self.attempts.saturating_add(1);
    }

    /// Process a key event.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> PasswordAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Ctrl+C cancels (matches OAuth modal).
        if ctrl && matches!(key.code, KeyCode::Char('c')) {
            return PasswordAction::Cancel;
        }
        // Ctrl+U clears the buffer.
        if ctrl && matches!(key.code, KeyCode::Char('u') | KeyCode::Char('U')) {
            self.buffer.clear();
            return PasswordAction::Continue;
        }

        match key.code {
            KeyCode::Esc => PasswordAction::Cancel,
            KeyCode::Enter => {
                // Take ownership of the buffer so the password is not
                // left lingering in the modal after submission.
                let pw = std::mem::take(&mut self.buffer);
                PasswordAction::Submit(pw)
            }
            KeyCode::Backspace => {
                // Pop one UTF-8 character (not one byte).
                self.buffer.pop();
                PasswordAction::Continue
            }
            KeyCode::Char(ch) => {
                // Reject control characters (other than the explicit
                // Ctrl+U/Ctrl+C handled above).
                if !ctrl && !ch.is_control() {
                    self.buffer.push(ch);
                }
                PasswordAction::Continue
            }
            _ => PasswordAction::Continue,
        }
    }

    /// Render the modal as a centered overlay.
    pub(crate) fn render(&self, frame: &mut Frame, area: Rect, accent: Color, error_color: Color) {
        let snap = self.render_snapshot();
        snap.render(frame, area, accent, error_color);
    }

    /// Build a Clone-able render snapshot.  The mask is produced here
    /// rather than holding the raw buffer in the snapshot so it stays
    /// out of the `LayoutSnapshot`.
    pub(crate) fn render_snapshot(&self) -> PasswordRenderSnapshot {
        PasswordRenderSnapshot {
            title: self.title.clone(),
            prompt: self.prompt.clone(),
            mask_len: self.buffer.chars().count(),
            error: self.error.clone(),
            attempts: self.attempts,
        }
    }
}

// ─── Render snapshot ─────────────────────────────────────────────────────────

/// Clone-able subset of `PasswordModal` for the draw closure.  Holds
/// only the masked length — never the raw buffer.
#[derive(Debug, Clone)]
pub(crate) struct PasswordRenderSnapshot {
    pub title: String,
    pub prompt: String,
    pub mask_len: usize,
    pub error: Option<String>,
    pub attempts: u32,
}

impl PasswordRenderSnapshot {
    pub fn render(&self, frame: &mut Frame, area: Rect, accent: Color, error_color: Color) {
        let modal_w = area.width.saturating_sub(8).min(60).max(30);
        let base_h: u16 = 9;
        let modal_h: u16 = base_h.saturating_add(if self.error.is_some() { 1 } else { 0 });
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

        frame.render_widget(Clear, modal_area);

        let border_color = if self.error.is_some() { error_color } else { accent };
        let block = Block::default()
            .title(format!(" {} ", self.title))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color).add_modifier(Modifier::BOLD))
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(modal_area);
        frame.render_widget(block, modal_area);

        let inner_w = inner.width as usize;
        // Cap the mask at the available inner width (minus a 2-col
        // padding) so a paste of a 200-char string doesn't blow the
        // popup out.
        let visible_mask_len = self.mask_len.min(inner_w.saturating_sub(4).max(8));
        let mask: String = std::iter::repeat_n('*', visible_mask_len).collect();
        let mask_display: String = if self.mask_len == 0 {
            String::new()
        } else if self.mask_len > visible_mask_len {
            // Indicate truncation with a leading ellipsis.
            let kept = visible_mask_len.saturating_sub(1);
            format!("…{}", "*".repeat(kept))
        } else {
            mask
        };

        let mut lines: Vec<Line<'static>> = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {}", self.prompt),
                Style::default().fg(super::modal_secondary_color()),
            )),
            Line::from(""),
            Line::from(Span::styled(
                format!(
                    "  {}",
                    if mask_display.is_empty() {
                        "<type password, Enter to submit>".to_string()
                    } else {
                        mask_display
                    }
                ),
                if self.mask_len == 0 {
                    Style::default().fg(super::modal_secondary_color())
                } else {
                    Style::default().fg(Color::Cyan)
                },
            )),
        ];

        if let Some(ref err) = self.error {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  {err}"),
                Style::default().fg(error_color),
            )));
        }

        // Pad so the hint sits at the bottom of the inner area.
        while lines.len() + 1 < inner.height as usize {
            lines.push(Line::from(""));
        }

        let hint = if self.attempts > 0 {
            format!(
                "  Enter: submit   Esc: cancel   Ctrl+U: clear   (attempt {})",
                self.attempts.saturating_add(1),
            )
        } else {
            "  Enter: submit   Esc: cancel   Ctrl+U: clear".to_string()
        };
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(super::modal_secondary_color()),
        )));

        let para = Paragraph::new(Text::from(lines));
        frame.render_widget(para, inner);
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn types_chars_into_buffer() {
        let mut modal = PasswordModal::new("Vault", "Master password");
        modal.handle_key(key(KeyCode::Char('h')));
        modal.handle_key(key(KeyCode::Char('i')));
        assert_eq!(modal.buffer, "hi");
    }

    #[test]
    fn backspace_pops_one_char() {
        let mut modal = PasswordModal::new("V", "P");
        modal.buffer = "hello".to_string();
        modal.handle_key(key(KeyCode::Backspace));
        assert_eq!(modal.buffer, "hell");
    }

    #[test]
    fn enter_submits_and_clears_buffer() {
        let mut modal = PasswordModal::new("V", "P");
        modal.buffer = "secret".to_string();
        match modal.handle_key(key(KeyCode::Enter)) {
            PasswordAction::Submit(pw) => {
                assert_eq!(pw, "secret");
                assert!(modal.buffer.is_empty(), "buffer should be cleared on submit");
            }
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn esc_cancels() {
        let mut modal = PasswordModal::new("V", "P");
        modal.buffer = "x".to_string();
        match modal.handle_key(key(KeyCode::Esc)) {
            PasswordAction::Cancel => {}
            other => panic!("expected Cancel, got {other:?}"),
        }
    }

    #[test]
    fn ctrl_u_clears_buffer() {
        let mut modal = PasswordModal::new("V", "P");
        modal.buffer = "wrongpw".to_string();
        modal.handle_key(ctrl(KeyCode::Char('u')));
        assert!(modal.buffer.is_empty());
    }

    #[test]
    fn ctrl_c_cancels() {
        let mut modal = PasswordModal::new("V", "P");
        modal.buffer = "x".to_string();
        match modal.handle_key(ctrl(KeyCode::Char('c'))) {
            PasswordAction::Cancel => {}
            other => panic!("expected Cancel, got {other:?}"),
        }
    }

    #[test]
    fn set_error_clears_buffer_and_increments_attempts() {
        let mut modal = PasswordModal::new("V", "P");
        modal.buffer = "wrong".to_string();
        modal.set_error("Wrong password");
        assert_eq!(modal.buffer, "");
        assert_eq!(modal.attempts, 1);
        assert_eq!(modal.error.as_deref(), Some("Wrong password"));
    }

    #[test]
    fn snapshot_carries_mask_length_not_buffer() {
        let mut modal = PasswordModal::new("V", "P");
        modal.buffer = "topsecret".to_string();
        let snap = modal.render_snapshot();
        assert_eq!(snap.mask_len, "topsecret".chars().count());
        // No raw buffer should be on the snapshot — only a length.
        // (Compile-time check: PasswordRenderSnapshot has no `buffer` field.)
    }

    #[test]
    fn snapshot_zero_attempts_by_default() {
        let modal = PasswordModal::new("V", "P");
        assert_eq!(modal.render_snapshot().attempts, 0);
    }

    #[test]
    fn render_in_small_terminal_does_not_panic() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(10, 5);
        let mut term = Terminal::new(backend).unwrap();
        let modal = PasswordModal::new("Vault", "Password");
        term.draw(|f| {
            modal.render(f, f.area(), Color::Cyan, Color::Red);
        })
        .unwrap();
    }

    #[test]
    fn render_with_error_does_not_panic() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let mut modal = PasswordModal::new("Vault", "Master password");
        modal.set_error("Wrong password — try again");
        term.draw(|f| {
            modal.render(f, f.area(), Color::Cyan, Color::Red);
        })
        .unwrap();
    }

    #[test]
    fn random_modifier_keys_do_not_enter_buffer() {
        let mut modal = PasswordModal::new("V", "P");
        // Tab is ignored (no mask append, no submit).
        modal.handle_key(key(KeyCode::Tab));
        assert!(modal.buffer.is_empty());
    }

    #[test]
    fn unicode_chars_count_in_mask_length() {
        let mut modal = PasswordModal::new("V", "P");
        modal.handle_key(key(KeyCode::Char('é')));
        modal.handle_key(key(KeyCode::Char('ñ')));
        let snap = modal.render_snapshot();
        assert_eq!(snap.mask_len, 2);
    }
}
