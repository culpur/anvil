//! `ConfirmModal` — reusable yes/no overlay (task #627).
//!
//! Replaces `print!("Restart? [y/N] "); read_line` style prompts that
//! corrupted the alt-screen back-buffer when run inside ratatui.  The
//! modal renders a centered popup with the title, a multi-line body,
//! and two buttons (Yes / No) with arrow-key highlight.
//!
//! ## Keys
//! - `y` / `Y`            commit Yes
//! - `n` / `N`            commit No
//! - `Tab` / `Left` / `Right`  toggle highlight
//! - `Enter`              commit current highlight
//! - `Esc`                cancel (returns `No`)
//!
//! ## Default
//! Defaults to `No` so an accidental `Enter` does not perform a
//! destructive action (matches the previous `[y/N]` prompt semantics).

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

/// Which choice is currently highlighted (or was committed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmChoice {
    Yes,
    No,
}

/// The outcome of `ConfirmModal::handle_key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmAction {
    /// Modal stays open; redraw and continue.
    Continue,
    /// User committed a choice (via `y`/`n`, Enter, or Esc).
    Committed(ConfirmChoice),
}

/// The yes/no confirmation modal.
#[derive(Debug, Clone)]
pub(crate) struct ConfirmModal {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) selected: ConfirmChoice,
}

impl ConfirmModal {
    /// Create a new modal with `No` selected by default.
    pub(crate) fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            selected: ConfirmChoice::No,
        }
    }

    /// Process a key event.  Returns `Committed(choice)` when the modal
    /// should close, `Continue` otherwise.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ConfirmAction {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.selected = ConfirmChoice::Yes;
                ConfirmAction::Committed(ConfirmChoice::Yes)
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.selected = ConfirmChoice::No;
                ConfirmAction::Committed(ConfirmChoice::No)
            }
            KeyCode::Esc => ConfirmAction::Committed(ConfirmChoice::No),
            KeyCode::Enter => ConfirmAction::Committed(self.selected),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                self.selected = match self.selected {
                    ConfirmChoice::Yes => ConfirmChoice::No,
                    ConfirmChoice::No => ConfirmChoice::Yes,
                };
                ConfirmAction::Continue
            }
            _ => ConfirmAction::Continue,
        }
    }

    /// Render the modal as a centered overlay.
    pub(crate) fn render(&self, frame: &mut Frame, area: Rect, accent: Color) {
        let snap = self.render_snapshot();
        snap.render(frame, area, accent);
    }

    /// Build a Clone-able render snapshot so the draw closure has no
    /// shared reference back to AnvilTui.
    pub(crate) fn render_snapshot(&self) -> ConfirmRenderSnapshot {
        ConfirmRenderSnapshot {
            title: self.title.clone(),
            body: self.body.clone(),
            selected: self.selected,
        }
    }
}

// ─── Render snapshot ─────────────────────────────────────────────────────────

/// Clone-able subset of `ConfirmModal` used by the draw closure.
#[derive(Debug, Clone)]
pub(crate) struct ConfirmRenderSnapshot {
    pub title: String,
    pub body: String,
    pub selected: ConfirmChoice,
}

impl ConfirmRenderSnapshot {
    /// Draw a centered popup at most 70 cols wide.  Caller passes the
    /// theme accent color so the highlight matches the rest of the TUI.
    pub fn render(&self, frame: &mut Frame, area: Rect, accent: Color) {
        // Width budget first; height adapts to the wrapped body so the
        // Yes/No buttons + key hint are never pushed off-screen when the
        // body is long (e.g. Ollama wizard's `already_running` modal with
        // a multi-model list).  Pre-task-#767 this was hardcoded to 9
        // rows; long bodies hid the buttons because the Paragraph clip
        // dropped the last 2 lines.
        let modal_w = area.width.saturating_sub(8).min(70).max(30);
        let usable_body_width = modal_w.saturating_sub(4).max(10) as usize;
        let body_rows = wrap_body(&self.body, usable_body_width).len() as u16;
        // 1 top pad + body + 1 spacer + 1 buttons + 1 hint + 2 borders = body + 6
        let needed_h: u16 = body_rows.saturating_add(6);
        // Clamp into the area; fall back to 9 if extremely small.
        let min_h: u16 = 9;
        let modal_h: u16 = needed_h.max(min_h).min(area.height.saturating_sub(2).max(min_h));
        if area.width < 12 || area.height < min_h {
            // Terminal too small — bail rather than crash.  The caller
            // can keep the modal open until the user resizes.
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

        let block = Block::default()
            .title(format!(" {} ", self.title))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(accent).add_modifier(Modifier::BOLD))
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(modal_area);
        frame.render_widget(block, modal_area);

        let body_lines = wrap_body(&self.body, inner.width as usize);
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(body_lines.len() + 3);
        lines.push(Line::from(""));
        for l in body_lines {
            lines.push(Line::from(Span::styled(
                format!("  {l}"),
                Style::default().fg(Color::White),
            )));
        }
        // Pad so the buttons sit at the bottom of the inner area.
        while lines.len() + 2 < inner.height as usize {
            lines.push(Line::from(""));
        }
        lines.push(button_row(self.selected, accent));
        lines.push(Line::from(Span::styled(
            "  y / n   Enter: confirm   Tab: switch   Esc: cancel",
            Style::default().fg(super::modal_secondary_color()),
        )));

        let para = Paragraph::new(Text::from(lines));
        frame.render_widget(para, inner);
    }
}

fn button_row(selected: ConfirmChoice, accent: Color) -> Line<'static> {
    let yes_style = if selected == ConfirmChoice::Yes {
        Style::default()
            .fg(Color::Black)
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let no_style = if selected == ConfirmChoice::No {
        Style::default()
            .fg(Color::Black)
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    Line::from(vec![
        Span::raw("    "),
        Span::styled("[ Yes ]", yes_style),
        Span::raw("    "),
        Span::styled("[ No ]", no_style),
    ])
}

/// Soft-wrap a body string to the inner width, splitting on whitespace
/// when possible and falling back to mid-word splits.  Never produces
/// lines wider than `width`, never produces empty lines from a single
/// space.
fn wrap_body(body: &str, width: usize) -> Vec<String> {
    let usable = width.saturating_sub(2).max(10);
    let mut out: Vec<String> = Vec::new();
    for raw_line in body.split('\n') {
        if raw_line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in raw_line.split_whitespace() {
            if current.is_empty() {
                if word.chars().count() <= usable {
                    current.push_str(word);
                } else {
                    // Long word: hard-split.
                    let mut buf = String::new();
                    for ch in word.chars() {
                        if buf.chars().count() + 1 > usable {
                            out.push(buf.clone());
                            buf.clear();
                        }
                        buf.push(ch);
                    }
                    current = buf;
                }
            } else if current.chars().count() + 1 + word.chars().count() <= usable {
                current.push(' ');
                current.push_str(word);
            } else {
                out.push(current.clone());
                current.clear();
                current.push_str(word);
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn defaults_to_no() {
        let modal = ConfirmModal::new("title", "body");
        assert_eq!(modal.selected, ConfirmChoice::No);
    }

    #[test]
    fn y_commits_yes() {
        let mut modal = ConfirmModal::new("t", "b");
        let act = modal.handle_key(key(KeyCode::Char('y')));
        assert_eq!(act, ConfirmAction::Committed(ConfirmChoice::Yes));
    }

    #[test]
    fn uppercase_y_commits_yes() {
        let mut modal = ConfirmModal::new("t", "b");
        let act = modal.handle_key(key(KeyCode::Char('Y')));
        assert_eq!(act, ConfirmAction::Committed(ConfirmChoice::Yes));
    }

    #[test]
    fn n_commits_no() {
        let mut modal = ConfirmModal::new("t", "b");
        let act = modal.handle_key(key(KeyCode::Char('n')));
        assert_eq!(act, ConfirmAction::Committed(ConfirmChoice::No));
    }

    #[test]
    fn uppercase_n_commits_no() {
        let mut modal = ConfirmModal::new("t", "b");
        let act = modal.handle_key(key(KeyCode::Char('N')));
        assert_eq!(act, ConfirmAction::Committed(ConfirmChoice::No));
    }

    #[test]
    fn esc_commits_no_even_when_yes_highlighted() {
        let mut modal = ConfirmModal::new("t", "b");
        modal.selected = ConfirmChoice::Yes;
        let act = modal.handle_key(key(KeyCode::Esc));
        assert_eq!(act, ConfirmAction::Committed(ConfirmChoice::No));
    }

    #[test]
    fn enter_commits_current_highlight() {
        let mut modal = ConfirmModal::new("t", "b");
        modal.selected = ConfirmChoice::Yes;
        let act = modal.handle_key(key(KeyCode::Enter));
        assert_eq!(act, ConfirmAction::Committed(ConfirmChoice::Yes));
    }

    #[test]
    fn enter_on_default_commits_no() {
        let mut modal = ConfirmModal::new("t", "b");
        let act = modal.handle_key(key(KeyCode::Enter));
        assert_eq!(act, ConfirmAction::Committed(ConfirmChoice::No));
    }

    #[test]
    fn tab_toggles() {
        let mut modal = ConfirmModal::new("t", "b");
        assert_eq!(modal.selected, ConfirmChoice::No);
        let act = modal.handle_key(key(KeyCode::Tab));
        assert_eq!(act, ConfirmAction::Continue);
        assert_eq!(modal.selected, ConfirmChoice::Yes);
        modal.handle_key(key(KeyCode::Tab));
        assert_eq!(modal.selected, ConfirmChoice::No);
    }

    #[test]
    fn left_right_toggle() {
        let mut modal = ConfirmModal::new("t", "b");
        modal.handle_key(key(KeyCode::Left));
        assert_eq!(modal.selected, ConfirmChoice::Yes);
        modal.handle_key(key(KeyCode::Right));
        assert_eq!(modal.selected, ConfirmChoice::No);
    }

    #[test]
    fn random_key_is_continue() {
        let mut modal = ConfirmModal::new("t", "b");
        let act = modal.handle_key(key(KeyCode::Char('z')));
        assert_eq!(act, ConfirmAction::Continue);
        assert_eq!(modal.selected, ConfirmChoice::No);
    }

    #[test]
    fn wrap_body_splits_on_spaces() {
        let out = wrap_body("hello world how are you", 12);
        assert!(out.iter().all(|l| l.chars().count() <= 10));
        assert!(!out.is_empty());
    }

    #[test]
    fn wrap_body_hard_splits_long_words() {
        let out = wrap_body("supercalifragilisticexpialidocious", 12);
        assert!(out.iter().all(|l| l.chars().count() <= 10));
    }

    #[test]
    fn wrap_body_preserves_blank_lines() {
        let out = wrap_body("first\n\nthird", 20);
        assert_eq!(out, vec!["first", "", "third"]);
    }

    #[test]
    fn render_in_small_terminal_does_not_panic() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        // Tiny terminal — the render method must early-return instead of
        // panicking with an out-of-bounds Rect.
        let backend = TestBackend::new(10, 5);
        let mut term = Terminal::new(backend).unwrap();
        let modal = ConfirmModal::new("t", "b");
        term.draw(|f| {
            modal.render(f, f.area(), Color::Cyan);
        })
        .unwrap();
    }

    #[test]
    fn render_in_standard_terminal_keeps_modal_within_bounds() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let modal = ConfirmModal::new("Restart Anvil?", "This will exit and respawn.");
        term.draw(|f| {
            let area = f.area();
            // Calling render must not paint outside `area`.
            modal.render(f, area, Color::Cyan);
            // Smoke check: area was the full backend.
            assert_eq!(area.width, 80);
            assert_eq!(area.height, 24);
        })
        .unwrap();
    }
}
