/// Key event processing, Ctrl key handlers, mouse events, paste handling,
/// input editing, history, and completion logic.
///
/// All items in this file are `impl AnvilTui` methods, implemented via the
/// `AnvilTui` struct defined in `mod.rs`.
use std::io;
use std::time::Duration;

use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::AnvilTui;
use super::ReadResult;
use super::helpers::{next_char_boundary, prev_char_boundary};
use super::state::CompletionPopup;
use super::widgets::{check_clipboard_for_image, update_completions};

impl AnvilTui {
    // ─── Main input loop ─────────────────────────────────────────────────────

    /// Run the interactive REPL loop.
    ///
    /// Returns `Ok(Some(input))` when the user submits a line.
    /// Returns `Ok(None)` when the user exits (`/exit`, Ctrl+C on empty, Ctrl+D).
    pub fn read_input(&mut self) -> io::Result<ReadResult> {
        self.active_tab_mut().think_frame = self.active_tab().think_frame.wrapping_add(1);
        self.draw()?;

        if event::poll(Duration::from_millis(80))? {
            match event::read()? {
                CtEvent::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                    return self.handle_key(key);
                }
                CtEvent::Paste(text) => {
                    let cleaned = text.replace('\r', "");
                    for ch in cleaned.chars() {
                        self.insert_char(ch);
                    }
                    self.refresh_completion();
                }
                CtEvent::Mouse(mouse) => {
                    match mouse.kind {
                        crossterm::event::MouseEventKind::ScrollUp => {
                            self.scroll_up(3);
                        }
                        crossterm::event::MouseEventKind::ScrollDown => {
                            self.scroll_down(3);
                        }
                        _ => {}
                    }
                }
                CtEvent::Resize(_, _) => {}
                _ => {}
            }
        }

        Ok(ReadResult::Continue)
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> io::Result<ReadResult> {
        use super::configure_types::ConfigureState;

        if self.configure_state != ConfigureState::Inactive {
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c' | 'C')) {
                    self.configure_state = ConfigureState::Inactive;
                    return Ok(ReadResult::Continue);
                }
            return self.handle_configure_key(key);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.handle_ctrl_key(key);
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            if let KeyCode::Char(ch) = key.code {
                if let Some(n) = ch.to_digit(10) {
                    if n >= 1 {
                        self.switch_tab((n as usize).saturating_sub(1));
                        return Ok(ReadResult::Continue);
                    }
                }
            }
        }

        match key.code {
            KeyCode::Enter => {
                if self.active_tab().completion.visible {
                    self.tab_complete();
                    self.active_tab_mut().completion = CompletionPopup::default();
                } else if let Some(line) = self.submit_input() {
                    self.active_tab_mut().completion = CompletionPopup::default();
                    return Ok(ReadResult::Submit(line));
                }
            }
            KeyCode::Backspace => {
                self.backspace();
                self.refresh_completion();
            }
            KeyCode::Delete => self.delete_char(),
            KeyCode::Left => self.cursor_left(),
            KeyCode::Right => self.cursor_right(),
            KeyCode::Home => self.cursor_home(),
            KeyCode::End => self.cursor_end(),
            KeyCode::Up => {
                if self.active_tab().completion.visible {
                    self.completion_up();
                } else if self.active_tab().think_label.is_empty() {
                    self.history_up();
                } else {
                    self.scroll_up(3);
                }
            }
            KeyCode::Down => {
                if self.active_tab().completion.visible {
                    self.completion_down();
                } else if self.active_tab().think_label.is_empty() {
                    self.history_down();
                } else {
                    self.scroll_down(3);
                }
            }
            KeyCode::PageUp => self.scroll_up(10),
            KeyCode::PageDown => self.scroll_down(10),
            KeyCode::Char(ch) => {
                self.insert_char(ch);
                self.refresh_completion();
            }
            KeyCode::Tab => {
                self.tab_complete();
            }
            KeyCode::Esc => {
                if self.active_tab().completion.visible {
                    self.active_tab_mut().completion = CompletionPopup::default();
                }
            }
            _ => {}
        }
        Ok(ReadResult::Continue)
    }

    pub(super) fn handle_ctrl_key(&mut self, key: KeyEvent) -> io::Result<ReadResult> {
        match key.code {
            KeyCode::Char('t' | 'T') => {
                return Ok(ReadResult::NewTab);
            }
            KeyCode::Char('w' | 'W') => {
                if self.tabs.len() > 1 {
                    if let Some(name) = self.close_active_tab() {
                        self.push_system(format!("Closed tab: {name}"));
                    }
                } else {
                    self.push_system("Cannot close the last tab.".to_string());
                }
            }
            KeyCode::Right => {
                self.next_tab();
            }
            KeyCode::Left => {
                self.prev_tab();
            }
            KeyCode::Char(']') => {
                self.next_tab();
            }
            KeyCode::Char('[') => {
                self.prev_tab();
            }
            KeyCode::Char('n' | 'N') if self.active_tab().input.is_empty() => {
                self.next_tab();
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() && ch != '0' => {
                let n = ch as usize - '0' as usize;
                self.switch_tab(n.saturating_sub(1));
            }
            KeyCode::Char('c' | 'C') => {
                if self.active_tab().input.is_empty() {
                    if let Some(first) = self.ctrl_c_empty_at {
                        if first.elapsed() <= Duration::from_secs(1) {
                            return Ok(ReadResult::Exit);
                        }
                    }
                    self.ctrl_c_empty_at = Some(std::time::Instant::now());
                    self.push_system(
                        "(press Ctrl+C again to exit)".to_string(),
                    );
                } else {
                    let tab = self.active_tab_mut();
                    tab.input.clear();
                    tab.cursor = 0;
                    tab.history_idx = None;
                    tab.history_backup = None;
                    self.ctrl_c_empty_at = None;
                }
            }
            KeyCode::Char('d' | 'D') => {
                if self.active_tab().input.is_empty() {
                    return Ok(ReadResult::Exit);
                }
                self.delete_char();
            }
            KeyCode::Char('u' | 'U') => {
                let cursor = self.active_tab().cursor;
                self.active_tab_mut().input.drain(..cursor);
                self.active_tab_mut().cursor = 0;
            }
            KeyCode::Char('k' | 'K') => {
                let cursor = self.active_tab().cursor;
                self.active_tab_mut().input.truncate(cursor);
            }
            KeyCode::Char('a' | 'A') => {
                if self.active_tab().input.is_empty() && !self.agent_rows.is_empty() {
                    self.agent_panel_visible = !self.agent_panel_visible;
                } else {
                    self.cursor_home();
                }
            }
            KeyCode::Char('e' | 'E') => self.cursor_end(),
            KeyCode::Char('j' | 'J') => {
                self.insert_char('\n');
            }
            KeyCode::Char('p' | 'P') => self.history_up(),
            KeyCode::Char('n' | 'N') => self.history_down(),
            KeyCode::Char('v' | 'V') => {
                if let Some(png_bytes) = check_clipboard_for_image() {
                    let tmp = std::env::temp_dir().join("anvil-paste.png");
                    if std::fs::write(&tmp, &png_bytes).is_ok() {
                        let path_str = tmp.to_string_lossy().to_string();
                        let snippet = format!("@{path_str}");
                        for ch in snippet.chars() {
                            self.insert_char(ch);
                        }
                        self.push_system(format!(
                            "Clipboard image ({} bytes) saved to {path_str} and referenced in input.",
                            png_bytes.len()
                        ));
                    }
                }
            }
            _ => {}
        }
        Ok(ReadResult::Continue)
    }

    // ─── Input editing ───────────────────────────────────────────────────────

    pub(super) fn insert_char(&mut self, ch: char) {
        let tab = self.active_tab_mut();
        tab.input.insert(tab.cursor, ch);
        tab.cursor += ch.len_utf8();
        tab.history_idx = None;
        tab.history_backup = None;
    }

    pub(super) fn backspace(&mut self) {
        if self.active_tab().cursor == 0 {
            return;
        }
        let (cursor, input) = {
            let tab = self.active_tab();
            (tab.cursor, tab.input.clone())
        };
        let prev = prev_char_boundary(&input, cursor);
        let tab = self.active_tab_mut();
        tab.input.drain(prev..cursor);
        tab.cursor = prev;
        tab.history_idx = None;
        tab.history_backup = None;
    }

    pub(super) fn delete_char(&mut self) {
        let (cursor, len) = {
            let tab = self.active_tab();
            (tab.cursor, tab.input.len())
        };
        if cursor >= len {
            return;
        }
        let next = {
            let input = self.active_tab().input.clone();
            next_char_boundary(&input, cursor)
        };
        self.active_tab_mut().input.drain(cursor..next);
    }

    pub(super) fn cursor_left(&mut self) {
        let (cursor, input) = {
            let tab = self.active_tab();
            (tab.cursor, tab.input.clone())
        };
        if cursor > 0 {
            self.active_tab_mut().cursor = prev_char_boundary(&input, cursor);
        }
    }

    pub(super) fn cursor_right(&mut self) {
        let (cursor, input) = {
            let tab = self.active_tab();
            (tab.cursor, tab.input.clone())
        };
        if cursor < input.len() {
            self.active_tab_mut().cursor = next_char_boundary(&input, cursor);
        }
    }

    pub(super) fn cursor_home(&mut self) {
        self.active_tab_mut().cursor = 0;
    }

    pub(super) fn cursor_end(&mut self) {
        let len = self.active_tab().input.len();
        self.active_tab_mut().cursor = len;
    }

    // ─── History navigation ──────────────────────────────────────────────────

    pub(super) fn history_up(&mut self) {
        if self.active_tab().history.is_empty() {
            return;
        }
        let (idx, len) = {
            let tab = self.active_tab();
            (tab.history_idx, tab.history.len())
        };
        match idx {
            None => {
                let new_idx = len - 1;
                let entry = self.active_tab().history[new_idx].clone();
                let tab = self.active_tab_mut();
                tab.history_backup = Some(tab.input.clone());
                tab.history_idx = Some(new_idx);
                tab.input = entry;
            }
            Some(0) => {}
            Some(i) => {
                let new_idx = i - 1;
                let entry = self.active_tab().history[new_idx].clone();
                let tab = self.active_tab_mut();
                tab.history_idx = Some(new_idx);
                tab.input = entry;
            }
        }
        let len = self.active_tab().input.len();
        self.active_tab_mut().cursor = len;
    }

    pub(super) fn history_down(&mut self) {
        let (idx, history_len) = {
            let tab = self.active_tab();
            (tab.history_idx, tab.history.len())
        };
        match idx {
            None => {}
            Some(i) => {
                if i + 1 >= history_len {
                    let backup = self.active_tab_mut().history_backup.take().unwrap_or_default();
                    let tab = self.active_tab_mut();
                    tab.history_idx = None;
                    tab.input = backup;
                } else {
                    let next_idx = i + 1;
                    let entry = self.active_tab().history[next_idx].clone();
                    let tab = self.active_tab_mut();
                    tab.history_idx = Some(next_idx);
                    tab.input = entry;
                }
                let len = self.active_tab().input.len();
                self.active_tab_mut().cursor = len;
            }
        }
    }

    // ─── Submit ──────────────────────────────────────────────────────────────

    pub(super) fn submit_input(&mut self) -> Option<String> {
        use super::state::LogEntry;
        let text = std::mem::take(&mut self.active_tab_mut().input);
        {
            let tab = self.active_tab_mut();
            tab.cursor = 0;
            tab.history_idx = None;
            tab.history_backup = None;
        }
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return None;
        }
        {
            let tab = self.active_tab_mut();
            tab.history.push(trimmed.clone());
            tab.log.push(LogEntry::User(trimmed.clone()));
        }
        self.scroll_to_bottom();
        Some(trimmed)
    }

    // ─── Completion ──────────────────────────────────────────────────────────

    pub(super) fn tab_complete(&mut self) {
        let (visible, matches_empty) = {
            let c = &self.active_tab().completion;
            (c.visible, c.matches.is_empty())
        };
        if !visible || matches_empty {
            let input = self.active_tab().input.clone();
            self.active_tab_mut().completion = update_completions(&input);
            return;
        }

        let (insert, first_part) = {
            let tab = &self.active_tab().completion;
            let selected = tab.matches[tab.selected].insert.clone();
            let input = self.active_tab().input.clone();
            let first = input.split(' ').next().unwrap_or("").to_string();
            (selected, first)
        };
        let input_clone = self.active_tab().input.clone();
        let word_count = input_clone.split_whitespace().count();
        let trailing_space = input_clone.ends_with(' ');
        let new_input = if word_count <= 1 && !trailing_space {
            format!("{insert} ")
        } else if word_count == 1 && trailing_space {
            let base = input_clone.trim_end();
            format!("{base} {insert} ")
        } else if word_count == 2 && trailing_space {
            let base = input_clone.trim_end();
            let cmd = input_clone.split_whitespace().next().unwrap_or("");
            let fourth = super::widgets::third_level_completions(cmd, &insert);
            if fourth.is_empty() {
                format!("{base} {insert}")
            } else {
                format!("{base} {insert} ")
            }
        } else if word_count == 2 && !trailing_space {
            let base = input_clone.split_whitespace().next().unwrap_or("");
            let cmd = base;
            let fourth = super::widgets::third_level_completions(cmd, &insert);
            if fourth.is_empty() {
                format!("{base} {insert}")
            } else {
                format!("{base} {insert} ")
            }
        } else if word_count == 3 && trailing_space {
            let base = input_clone.trim_end();
            format!("{base} {insert}")
        } else if word_count >= 3 {
            let parts: Vec<&str> = input_clone.split_whitespace().collect();
            let base = parts[..parts.len()-1].join(" ");
            format!("{base} {insert}")
        } else {
            format!("{first_part} {insert}")
        };
        let new_len = new_input.len();
        let tab = self.active_tab_mut();
        tab.input = new_input.clone();
        tab.cursor = new_len;
        tab.completion = update_completions(&new_input);
    }

    pub(super) fn completion_up(&mut self) {
        let tab = self.active_tab_mut();
        if tab.completion.visible && !tab.completion.matches.is_empty() {
            if tab.completion.selected > 0 {
                tab.completion.selected -= 1;
            } else {
                tab.completion.selected = tab.completion.matches.len() - 1;
            }
        }
    }

    pub(super) fn completion_down(&mut self) {
        let tab = self.active_tab_mut();
        if tab.completion.visible && !tab.completion.matches.is_empty() {
            if tab.completion.selected + 1 < tab.completion.matches.len() {
                tab.completion.selected += 1;
            } else {
                tab.completion.selected = 0;
            }
        }
    }

    pub(super) fn refresh_completion(&mut self) {
        let input = self.active_tab().input.clone();
        self.active_tab_mut().completion = update_completions(&input);
    }
}
