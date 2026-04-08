pub mod completion;
pub mod history;
pub mod line_editor;

use std::io::{self, IsTerminal, Write};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal;

use completion::{complete_slash_command, CompletionState};
use history::{history_down, history_up};
use line_editor::{
    current_line_delete_range, line_end, move_vertical, next_boundary, previous_boundary,
    previous_command_boundary, remove_previous_char, EditSession, EditorMode, YankBuffer,
};

// ─── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    Submit(String),
    Cancel,
    Exit,
}

// ─── Internal types ───────────────────────────────────────────────────────────

enum KeyAction {
    Continue,
    Submit(String),
    Cancel,
    Exit,
    ToggleVim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Submission {
    Submit,
    ToggleVim,
}

// ─── LineEditor ───────────────────────────────────────────────────────────────

pub struct LineEditor {
    prompt: String,
    completions: Vec<String>,
    history: Vec<String>,
    yank_buffer: YankBuffer,
    vim_enabled: bool,
    completion_state: Option<CompletionState>,
}

impl LineEditor {
    #[must_use]
    pub fn new(prompt: impl Into<String>, completions: Vec<String>) -> Self {
        Self {
            prompt: prompt.into(),
            completions,
            history: Vec::new(),
            yank_buffer: YankBuffer::default(),
            vim_enabled: false,
            completion_state: None,
        }
    }

    /// Toggle vim keybindings and return the new state.
    pub fn toggle_vim(&mut self) -> bool {
        self.vim_enabled = !self.vim_enabled;
        self.vim_enabled
    }

    /// Return whether vim mode is currently enabled.
    #[must_use]
    pub fn is_vim_enabled(&self) -> bool {
        self.vim_enabled
    }

    pub fn push_history(&mut self, entry: impl Into<String>) {
        let entry = entry.into();
        if entry.trim().is_empty() {
            return;
        }

        self.history.push(entry);
    }

    pub fn read_line(&mut self) -> io::Result<ReadOutcome> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return self.read_line_fallback();
        }

        let _raw_mode = RawModeGuard::new()?;
        let mut stdout = io::stdout();
        let mut session = EditSession::new(self.vim_enabled);
        session.render(&mut stdout, &self.prompt, self.vim_enabled)?;

        loop {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                continue;
            }

            match self.handle_key_event(&mut session, key) {
                KeyAction::Continue => {
                    session.render(&mut stdout, &self.prompt, self.vim_enabled)?;
                }
                KeyAction::Submit(line) => {
                    session.finalize_render(&mut stdout, &self.prompt, self.vim_enabled)?;
                    return Ok(ReadOutcome::Submit(line));
                }
                KeyAction::Cancel => {
                    session.clear_render(&mut stdout)?;
                    writeln!(stdout)?;
                    return Ok(ReadOutcome::Cancel);
                }
                KeyAction::Exit => {
                    session.clear_render(&mut stdout)?;
                    writeln!(stdout)?;
                    return Ok(ReadOutcome::Exit);
                }
                KeyAction::ToggleVim => {
                    session.clear_render(&mut stdout)?;
                    self.vim_enabled = !self.vim_enabled;
                    writeln!(
                        stdout,
                        "Vim mode {}.",
                        if self.vim_enabled {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    )?;
                    session = EditSession::new(self.vim_enabled);
                    session.render(&mut stdout, &self.prompt, self.vim_enabled)?;
                }
            }
        }
    }

    fn read_line_fallback(&mut self) -> io::Result<ReadOutcome> {
        loop {
            let mut stdout = io::stdout();
            write!(stdout, "{}", self.prompt)?;
            stdout.flush()?;

            let mut buffer = String::new();
            let bytes_read = io::stdin().read_line(&mut buffer)?;
            if bytes_read == 0 {
                return Ok(ReadOutcome::Exit);
            }

            while matches!(buffer.chars().last(), Some('\n' | '\r')) {
                buffer.pop();
            }

            if self.handle_submission(&buffer) == Submission::ToggleVim {
                self.vim_enabled = !self.vim_enabled;
                writeln!(
                    stdout,
                    "Vim mode {}.",
                    if self.vim_enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                )?;
                continue;
            }

            return Ok(ReadOutcome::Submit(buffer));
        }
    }

    fn handle_key_event(&mut self, session: &mut EditSession, key: KeyEvent) -> KeyAction {
        if key.code != KeyCode::Tab {
            self.completion_state = None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c' | 'C') => {
                    return if session.has_input() {
                        KeyAction::Cancel
                    } else {
                        KeyAction::Exit
                    };
                }
                KeyCode::Char('j' | 'J') => {
                    if session.mode != EditorMode::Normal && session.mode != EditorMode::Visual {
                        self.insert_active_text(session, "\n");
                    }
                    return KeyAction::Continue;
                }
                KeyCode::Char('d' | 'D') => {
                    if session.current_len() == 0 {
                        return KeyAction::Exit;
                    }
                    self.delete_char_under_cursor(session);
                    return KeyAction::Continue;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                if session.mode != EditorMode::Normal && session.mode != EditorMode::Visual {
                    self.insert_active_text(session, "\n");
                }
                KeyAction::Continue
            }
            KeyCode::Enter => self.submit_or_toggle(session),
            KeyCode::Esc => self.handle_escape(session),
            KeyCode::Backspace => {
                self.handle_backspace(session);
                KeyAction::Continue
            }
            KeyCode::Delete => {
                self.delete_char_under_cursor(session);
                KeyAction::Continue
            }
            KeyCode::Left => {
                self.move_left(session);
                KeyAction::Continue
            }
            KeyCode::Right => {
                self.move_right(session);
                KeyAction::Continue
            }
            KeyCode::Up => {
                history_up(&self.history, self.vim_enabled, session);
                KeyAction::Continue
            }
            KeyCode::Down => {
                history_down(&self.history, self.vim_enabled, session);
                KeyAction::Continue
            }
            KeyCode::Home => {
                self.move_line_start(session);
                KeyAction::Continue
            }
            KeyCode::End => {
                self.move_line_end(session);
                KeyAction::Continue
            }
            KeyCode::Tab => {
                complete_slash_command(&self.completions, &mut self.completion_state, session);
                KeyAction::Continue
            }
            KeyCode::Char(ch) => {
                self.handle_char(session, ch);
                KeyAction::Continue
            }
            _ => KeyAction::Continue,
        }
    }

    fn handle_char(&mut self, session: &mut EditSession, ch: char) {
        match session.mode {
            EditorMode::Plain | EditorMode::Insert | EditorMode::Command => {
                self.insert_active_char(session, ch);
            }
            EditorMode::Normal => self.handle_normal_char(session, ch),
            EditorMode::Visual => self.handle_visual_char(session, ch),
        }
    }

    fn handle_normal_char(&mut self, session: &mut EditSession, ch: char) {
        if let Some(operator) = session.pending_operator.take() {
            match (operator, ch) {
                ('d', 'd') => {
                    self.delete_current_line(session);
                    return;
                }
                ('y', 'y') => {
                    self.yank_current_line(session);
                    return;
                }
                _ => {}
            }
        }

        match ch {
            'h' => self.move_left(session),
            'j' => self.move_down(session),
            'k' => self.move_up(session),
            'l' => self.move_right(session),
            'd' | 'y' => session.pending_operator = Some(ch),
            'p' => self.paste_after(session),
            'i' => session.enter_insert_mode(),
            'v' => session.enter_visual_mode(),
            ':' => session.enter_command_mode(),
            _ => {}
        }
    }

    fn handle_visual_char(&mut self, session: &mut EditSession, ch: char) {
        match ch {
            'h' => self.move_left(session),
            'j' => self.move_down(session),
            'k' => self.move_up(session),
            'l' => self.move_right(session),
            'v' => session.enter_normal_mode(),
            _ => {}
        }
    }

    fn handle_escape(&mut self, session: &mut EditSession) -> KeyAction {
        match session.mode {
            EditorMode::Plain | EditorMode::Normal => KeyAction::Continue,
            EditorMode::Insert => {
                if session.cursor > 0 {
                    session.cursor = previous_boundary(&session.text, session.cursor);
                }
                session.enter_normal_mode();
                KeyAction::Continue
            }
            EditorMode::Visual => {
                session.enter_normal_mode();
                KeyAction::Continue
            }
            EditorMode::Command => {
                session.exit_command_mode();
                KeyAction::Continue
            }
        }
    }

    fn handle_backspace(&mut self, session: &mut EditSession) {
        match session.mode {
            EditorMode::Normal | EditorMode::Visual => self.move_left(session),
            EditorMode::Command => {
                if session.command_cursor <= 1 {
                    session.exit_command_mode();
                } else {
                    remove_previous_char(&mut session.command_buffer, &mut session.command_cursor);
                }
            }
            EditorMode::Plain | EditorMode::Insert => {
                remove_previous_char(&mut session.text, &mut session.cursor);
            }
        }
    }

    fn submit_or_toggle(&mut self, session: &EditSession) -> KeyAction {
        let line = session.current_line();
        match self.handle_submission(&line) {
            Submission::Submit => KeyAction::Submit(line),
            Submission::ToggleVim => KeyAction::ToggleVim,
        }
    }

    fn handle_submission(&mut self, line: &str) -> Submission {
        if line.trim() == "/vim" {
            Submission::ToggleVim
        } else {
            Submission::Submit
        }
    }

    fn insert_active_char(&mut self, session: &mut EditSession, ch: char) {
        let mut buffer = [0; 4];
        self.insert_active_text(session, ch.encode_utf8(&mut buffer));
    }

    fn insert_active_text(&mut self, session: &mut EditSession, text: &str) {
        if session.mode == EditorMode::Command {
            session
                .command_buffer
                .insert_str(session.command_cursor, text);
            session.command_cursor += text.len();
        } else {
            session.text.insert_str(session.cursor, text);
            session.cursor += text.len();
        }
    }

    fn move_left(&self, session: &mut EditSession) {
        if session.mode == EditorMode::Command {
            session.command_cursor =
                previous_command_boundary(&session.command_buffer, session.command_cursor);
        } else {
            session.cursor = previous_boundary(&session.text, session.cursor);
        }
    }

    fn move_right(&self, session: &mut EditSession) {
        if session.mode == EditorMode::Command {
            session.command_cursor =
                next_boundary(&session.command_buffer, session.command_cursor);
        } else {
            session.cursor = next_boundary(&session.text, session.cursor);
        }
    }

    fn move_line_start(&self, session: &mut EditSession) {
        if session.mode == EditorMode::Command {
            session.command_cursor = 1;
        } else {
            session.cursor = line_editor::line_start(&session.text, session.cursor);
        }
    }

    fn move_line_end(&self, session: &mut EditSession) {
        if session.mode == EditorMode::Command {
            session.command_cursor = session.command_buffer.len();
        } else {
            session.cursor = line_end(&session.text, session.cursor);
        }
    }

    fn move_up(&self, session: &mut EditSession) {
        if session.mode == EditorMode::Command {
            return;
        }
        session.cursor = move_vertical(&session.text, session.cursor, -1);
    }

    fn move_down(&self, session: &mut EditSession) {
        if session.mode == EditorMode::Command {
            return;
        }
        session.cursor = move_vertical(&session.text, session.cursor, 1);
    }

    fn delete_char_under_cursor(&self, session: &mut EditSession) {
        match session.mode {
            EditorMode::Command => {
                if session.command_cursor < session.command_buffer.len() {
                    let end = next_boundary(&session.command_buffer, session.command_cursor);
                    session.command_buffer.drain(session.command_cursor..end);
                }
            }
            _ => {
                if session.cursor < session.text.len() {
                    let end = next_boundary(&session.text, session.cursor);
                    session.text.drain(session.cursor..end);
                }
            }
        }
    }

    fn delete_current_line(&mut self, session: &mut EditSession) {
        let (line_start_idx, line_end_idx, delete_start_idx) =
            current_line_delete_range(&session.text, session.cursor);
        self.yank_buffer.text = session.text[line_start_idx..line_end_idx].to_string();
        self.yank_buffer.linewise = true;
        session.text.drain(delete_start_idx..line_end_idx);
        session.cursor = delete_start_idx.min(session.text.len());
    }

    fn yank_current_line(&mut self, session: &mut EditSession) {
        let (line_start_idx, line_end_idx, _) =
            current_line_delete_range(&session.text, session.cursor);
        self.yank_buffer.text = session.text[line_start_idx..line_end_idx].to_string();
        self.yank_buffer.linewise = true;
    }

    fn paste_after(&mut self, session: &mut EditSession) {
        if self.yank_buffer.text.is_empty() {
            return;
        }

        if self.yank_buffer.linewise {
            let line_end_idx = line_end(&session.text, session.cursor);
            let insert_at = if line_end_idx < session.text.len() {
                line_end_idx + 1
            } else {
                session.text.len()
            };
            let mut insertion = self.yank_buffer.text.clone();
            if insert_at == session.text.len()
                && !session.text.is_empty()
                && !session.text.ends_with('\n')
            {
                insertion.insert(0, '\n');
            }
            if insert_at < session.text.len() && !insertion.ends_with('\n') {
                insertion.push('\n');
            }
            session.text.insert_str(insert_at, &insertion);
            session.cursor = if insertion.starts_with('\n') {
                insert_at + 1
            } else {
                insert_at
            };
            return;
        }

        let insert_at = next_boundary(&session.text, session.cursor);
        session.text.insert_str(insert_at, &self.yank_buffer.text);
        session.cursor = insert_at + self.yank_buffer.text.len();
    }

    #[allow(dead_code)]
    fn history_up(&self, session: &mut EditSession) {
        history_up(&self.history, self.vim_enabled, session);
    }

    #[allow(dead_code)]
    fn history_down(&self, session: &mut EditSession) {
        history_down(&self.history, self.vim_enabled, session);
    }
}

// ─── Raw mode guard ───────────────────────────────────────────────────────────

struct RawModeGuard;

impl RawModeGuard {
    fn new() -> io::Result<Self> {
        terminal::enable_raw_mode().map_err(io::Error::other)?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        completion::slash_command_prefix, line_editor::{selection_bounds, EditSession, EditorMode},
        KeyAction, LineEditor,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn extracts_only_terminal_slash_command_prefixes() {
        // given
        let complete_prefix = slash_command_prefix("/he", 3);
        let whitespace_prefix = slash_command_prefix("/help me", 5);
        let plain_text_prefix = slash_command_prefix("hello", 5);
        let mid_buffer_prefix = slash_command_prefix("/help", 2);

        // when
        let result = (
            complete_prefix,
            whitespace_prefix,
            plain_text_prefix,
            mid_buffer_prefix,
        );

        // then
        assert_eq!(result, (Some("/he"), None, None, None));
    }

    #[test]
    fn toggle_submission_flips_vim_mode() {
        // given
        let mut editor = LineEditor::new("> ", vec!["/help".to_string(), "/vim".to_string()]);

        // when
        let first = editor.handle_submission("/vim");
        editor.vim_enabled = true;
        let second = editor.handle_submission("/vim");

        // then
        assert!(matches!(first, super::Submission::ToggleVim));
        assert!(matches!(second, super::Submission::ToggleVim));
    }

    #[test]
    fn normal_mode_supports_motion_and_insert_transition() {
        // given
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "hello".to_string();
        session.cursor = session.text.len();
        let _ = editor.handle_escape(&mut session);

        // when
        editor.handle_char(&mut session, 'h');
        editor.handle_char(&mut session, 'i');
        editor.handle_char(&mut session, '!');

        // then
        assert_eq!(session.mode, EditorMode::Insert);
        assert_eq!(session.text, "hel!lo");
    }

    #[test]
    fn yy_and_p_paste_yanked_line_after_current_line() {
        // given
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "alpha\nbeta\ngamma".to_string();
        session.cursor = 0;
        let _ = editor.handle_escape(&mut session);

        // when
        editor.handle_char(&mut session, 'y');
        editor.handle_char(&mut session, 'y');
        editor.handle_char(&mut session, 'p');

        // then
        assert_eq!(session.text, "alpha\nalpha\nbeta\ngamma");
    }

    #[test]
    fn dd_and_p_paste_deleted_line_after_current_line() {
        // given
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "alpha\nbeta\ngamma".to_string();
        session.cursor = 0;
        let _ = editor.handle_escape(&mut session);

        // when
        editor.handle_char(&mut session, 'j');
        editor.handle_char(&mut session, 'd');
        editor.handle_char(&mut session, 'd');
        editor.handle_char(&mut session, 'p');

        // then
        assert_eq!(session.text, "alpha\ngamma\nbeta\n");
    }

    #[test]
    fn visual_mode_tracks_selection_with_motions() {
        // given
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "alpha\nbeta".to_string();
        session.cursor = 0;
        let _ = editor.handle_escape(&mut session);

        // when
        editor.handle_char(&mut session, 'v');
        editor.handle_char(&mut session, 'j');
        editor.handle_char(&mut session, 'l');

        // then
        assert_eq!(session.mode, EditorMode::Visual);
        assert_eq!(
            selection_bounds(
                &session.text,
                session.visual_anchor.unwrap_or(0),
                session.cursor
            ),
            Some((0, 8))
        );
    }

    #[test]
    fn command_mode_submits_colon_prefixed_input() {
        // given
        let mut editor = LineEditor::new("> ", vec![]);
        editor.vim_enabled = true;
        let mut session = EditSession::new(true);
        session.text = "draft".to_string();
        session.cursor = session.text.len();
        let _ = editor.handle_escape(&mut session);

        // when
        editor.handle_char(&mut session, ':');
        editor.handle_char(&mut session, 'q');
        editor.handle_char(&mut session, '!');
        let action = editor.submit_or_toggle(&session);

        // then
        assert_eq!(session.mode, EditorMode::Command);
        assert_eq!(session.command_buffer, ":q!");
        assert!(matches!(action, KeyAction::Submit(line) if line == ":q!"));
    }

    #[test]
    fn push_history_ignores_blank_entries() {
        // given
        let mut editor = LineEditor::new("> ", vec!["/help".to_string()]);

        // when
        editor.push_history("   ");
        editor.push_history("/help");

        // then
        assert_eq!(editor.history, vec!["/help".to_string()]);
    }

    #[test]
    fn tab_completes_matching_slash_commands() {
        // given
        let mut editor = LineEditor::new("> ", vec!["/help".to_string(), "/hello".to_string()]);
        let mut session = EditSession::new(false);
        session.text = "/he".to_string();
        session.cursor = session.text.len();

        // when
        super::complete_slash_command(&editor.completions, &mut editor.completion_state, &mut session);

        // then
        assert_eq!(session.text, "/help");
        assert_eq!(session.cursor, 5);
    }

    #[test]
    fn tab_cycles_between_matching_slash_commands() {
        // given
        let mut editor = LineEditor::new(
            "> ",
            vec!["/permissions".to_string(), "/plugin".to_string()],
        );
        let mut session = EditSession::new(false);
        session.text = "/p".to_string();
        session.cursor = session.text.len();

        // when
        super::complete_slash_command(&editor.completions, &mut editor.completion_state, &mut session);
        let first = session.text.clone();
        session.cursor = session.text.len();
        super::complete_slash_command(&editor.completions, &mut editor.completion_state, &mut session);
        let second = session.text.clone();

        // then
        assert_eq!(first, "/permissions");
        assert_eq!(second, "/plugin");
    }

    #[test]
    fn ctrl_c_cancels_when_input_exists() {
        // given
        let mut editor = LineEditor::new("> ", vec![]);
        let mut session = EditSession::new(false);
        session.text = "draft".to_string();
        session.cursor = session.text.len();

        // when
        let action = editor.handle_key_event(
            &mut session,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );

        // then
        assert!(matches!(action, KeyAction::Cancel));
    }
}
