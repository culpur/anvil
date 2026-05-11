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
use super::widgets::{check_clipboard_for_image, has_further_completions, update_completions};

impl AnvilTui {
    // ─── Main input loop ─────────────────────────────────────────────────────

    /// Run the interactive REPL loop.
    ///
    /// Returns `Ok(Some(input))` when the user submits a line.
    /// Returns `Ok(None)` when the user exits (`/exit`, Ctrl+C on empty, Ctrl+D).
    pub fn read_input(&mut self) -> io::Result<ReadResult> {
        // Drain any auto-submission queued by the previous turn's in-flight
        // handler (e.g. a slash command typed while the model was streaming).
        // We return immediately so the caller dispatches it the same way as
        // a normal Enter-on-the-input-line submission.
        if let Some(line) = self.pending_auto_submit.take() {
            return Ok(ReadResult::Submit(line));
        }

        self.active_tab_mut().think_frame = self.active_tab().think_frame.wrapping_add(1);

        // T5-Ssh-D: drain all SSH tabs each frame, capped at 64 KB per tab to
        // prevent a noisy remote (e.g. `cat /dev/urandom`) from starving the UI.
        {
            const MAX_BYTES_PER_FRAME: usize = 64 * 1024;
            let mut needs_redraw = false;
            for tab in &mut self.tabs {
                if let Some(ref mut ssh) = tab.ssh {
                    let mut bytes_drained: usize = 0;
                    let mut got_stdout = false;
                    while let Ok(chunk) = ssh.stdout_rx.try_recv() {
                        bytes_drained += chunk.len();
                        ssh.parser.process(&chunk);
                        got_stdout = true;
                        if bytes_drained >= MAX_BYTES_PER_FRAME {
                            break;
                        }
                    }
                    let got_event = ssh.drain_events();
                    if got_stdout || got_event {
                        needs_redraw = true;
                    }
                }
            }
            if needs_redraw {
                self.redraw.request(super::redraw::DirtyRegions::SCROLLBACK);
            }
        }

        // Propagate terminal resize to all active SSH tabs.
        if let Ok(size) = crossterm::terminal::size() {
            let (cols, rows) = size;
            for tab in &mut self.tabs {
                if let Some(ref mut ssh) = tab.ssh {
                    // Use the full terminal size as an approximation; the exact
                    // inner pane dimensions differ by the header/footer, but this
                    // is close enough and avoids a layout re-run here.
                    let _ = ssh.resize_tx.send((u32::from(cols), u32::from(rows)));
                    ssh.parser.screen_mut().set_size(rows, cols);
                }
            }
        }

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
                    // Bug A fix: Shift+Drag events are passed through — the terminal
                    // emulator (Windows Terminal, ConEmu, most Linux VTEs, iTerm2)
                    // intercepts them for native text selection when the application
                    // does not consume them.  By explicitly not handling Shift+Drag
                    // here, we allow the emulator to perform selection.
                    //
                    // Note: cmd.exe does not reliably report the Shift modifier on
                    // drag events.  Users on cmd.exe should upgrade to Windows
                    // Terminal for native selection support.
                    if matches!(mouse.kind, crossterm::event::MouseEventKind::Drag(_))
                        && mouse.modifiers.contains(crossterm::event::KeyModifiers::SHIFT)
                    {
                        // Pass through — do not consume.
                    } else {
                        // Mouse-wheel routing: when the configure overlay is
                        // open, the wheel scrolls the overlay's list viewport
                        // (FEAT-36) so long pickers no longer truncate.
                        // Otherwise it falls through to the chat scrollback.
                        let in_configure = self.configure_state
                            != super::configure_types::ConfigureState::Inactive;
                        match mouse.kind {
                            crossterm::event::MouseEventKind::ScrollUp => {
                                if in_configure {
                                    self.configure_scroll_wheel(-3);
                                } else {
                                    self.scroll_up(3);
                                }
                            }
                            crossterm::event::MouseEventKind::ScrollDown => {
                                if in_configure {
                                    self.configure_scroll_wheel(3);
                                } else {
                                    self.scroll_down(3);
                                }
                            }
                            // Click-to-switch on the tab bar. We compare against
                            // the geometry recorded by the most-recent draw.
                            crossterm::event::MouseEventKind::Down(
                                crossterm::event::MouseButton::Left,
                            ) => {
                                if mouse.row == self.tab_bar_row {
                                    let col = mouse.column;
                                    let mut handled = false;
                                    // Search a copy of the geometry so we can
                                    // mutate self inside the loop.
                                    let hits = self.tab_hits.clone();
                                    for hit in &hits {
                                        if Some(col) == hit.close_col {
                                            if self.tabs.len() > 1 {
                                                self.switch_tab(hit.idx);
                                                if let Some(name) = self.close_active_tab() {
                                                    self.push_system(format!(
                                                        "Closed tab: {name}"
                                                    ));
                                                }
                                            }
                                            handled = true;
                                            break;
                                        }
                                        if col >= hit.label_start && col < hit.label_end {
                                            self.switch_tab(hit.idx);
                                            handled = true;
                                            break;
                                        }
                                    }
                                    if handled {
                                        self.redraw.request(
                                            super::redraw::DirtyRegions::ALL,
                                        );
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(ReadResult::Continue)
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> io::Result<ReadResult> {
        use super::configure_types::ConfigureState;

        // Bug-3 Commit 4: when the active tab has a pending permission request,
        // intercept the key and route it to the approval modal.  All other keys
        // are consumed (the modal is modal — no editing below it).
        {
            let active_tab_id = self.tabs.get(self.active_tab).map(|t| t.id).unwrap_or(0);
            if self.pending_permissions.contains_key(&active_tab_id) {
                use crossterm::event::KeyCode as KC;
                let reply = match key.code {
                    KC::Char('y') | KC::Char('Y') | KC::Enter => {
                        Some(super::PermissionReply::Allow)
                    }
                    KC::Char('a') | KC::Char('A') => {
                        Some(super::PermissionReply::AllowAlways)
                    }
                    KC::Char('n') | KC::Char('N') | KC::Esc => {
                        Some(super::PermissionReply::Deny)
                    }
                    _ => None,
                };
                if let Some(r) = reply {
                    if let Some(pending) = self.pending_permissions.remove(&active_tab_id) {
                        let _ = pending.response_tx.send(r);
                    }
                    self.redraw.request(super::redraw::DirtyRegions::ALL);
                }
                return Ok(ReadResult::Continue);
            }
        }

        // T5-Ssh-E: when the SSH form modal is open, all keys go to the form.
        // Submit closes the modal and (in Commit F) kicks off the connection.
        // Cancel closes the modal silently.
        if self.ssh_form.is_some() {
            use super::ssh_form::SshFormResult;
            let result = self.ssh_form.as_mut().unwrap().handle_key(key);
            match result {
                None => {
                    // Form consumed the key; nothing to do.
                    self.redraw.request(super::redraw::DirtyRegions::SCROLLBACK);
                    return Ok(ReadResult::Continue);
                }
                Some(SshFormResult::Cancelled) => {
                    self.ssh_form = None;
                    self.push_system("SSH form cancelled.".to_string());
                    self.redraw.request(super::redraw::DirtyRegions::SCROLLBACK);
                    return Ok(ReadResult::Continue);
                }
                Some(SshFormResult::Submit(config, alias)) => {
                    self.ssh_form = None;
                    let dest = format!("{}@{}:{}", config.user, config.host, config.port);
                    // T5-Ssh-F: spawn the bridge and attach the SSH tab.
                    let (cols, rows) = crossterm::terminal::size()
                        .unwrap_or((220, 50));
                    let channels = super::ssh_bridge::spawn_session(
                        config.clone(),
                        (u32::from(cols), u32::from(rows)),
                    );
                    let ssh_state = super::ssh_tab::SshTabState::new(
                        dest.clone(),
                        cols,
                        rows,
                        channels.stdout_rx,
                        channels.stdin_tx,
                        channels.resize_tx,
                        channels.events_rx,
                    );
                    // Create a new tab for this SSH connection. SSH tabs use a
                    // dummy model string and an empty session_id — they carry no
                    // AI context.
                    let tab_name = format!("ssh:{dest}");
                    let idx = self.new_tab(tab_name, "ssh", "");
                    self.tabs[idx].ssh = Some(ssh_state);
                    self.switch_tab(idx);
                    self.push_system(format!("SSH connecting to {dest}…"));
                    // Optionally save the alias to the vault. Failures are
                    // non-fatal — missing vault or locked vault just silently
                    // skips the save.
                    if let Some(ref name) = alias {
                        if runtime::vault_is_session_unlocked() {
                            let name_copy = name.clone();
                            let _ = runtime::with_session_vault(|vm| {
                                runtime::ssh::save_ssh_alias(vm, &name_copy, &config)
                                    .map_err(|e| runtime::vault::VaultError::Serialization(e.to_string()))
                            });
                        }
                    }
                    self.redraw.request(super::redraw::DirtyRegions::SCROLLBACK);
                    return Ok(ReadResult::Continue);
                }
            }
        }

        // T5-Ssh-D: key forwarding for active SSH tabs.
        //
        // Ctrl+B is the SSH-mode escape prefix (matching screen/tmux convention).
        // It is never forwarded to the remote shell. Instead:
        //   - Ctrl+B alone:      sets ssh_escape_pending = true and returns.
        //   - While pending, digit '0'–'9': switch to that tab index and clear.
        //   - While pending, 'q': close the SSH tab (drops SshTabState; the
        //     bridge thread exits when its channels drop).
        //   - While pending, any other key: clear and forward normally.
        //
        // All other keys while ssh.is_some() are encoded via key_event_to_bytes
        // and forwarded to the remote shell; this function then returns Continue
        // so the normal chat-mode handlers don't also process the key.
        if self.active_tab().ssh.is_some() {
            let is_ctrl_b = key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('b' | 'B'));

            if self.ssh_escape_pending {
                self.ssh_escape_pending = false;
                match key.code {
                    KeyCode::Char(ch) if ch.is_ascii_digit() => {
                        let n = ch as usize - '0' as usize;
                        self.switch_tab(n.saturating_sub(1));
                        return Ok(ReadResult::Continue);
                    }
                    KeyCode::Char('q' | 'Q') => {
                        // Close the SSH tab: drop SshTabState. The bridge's
                        // channels close and the tokio thread exits naturally.
                        self.active_tab_mut().ssh = None;
                        self.push_system("SSH tab closed.".to_string());
                        return Ok(ReadResult::Continue);
                    }
                    _ => {
                        // Any other key after Ctrl+B: treat as if Ctrl+B was not
                        // pressed — fall through to normal forward path below.
                    }
                }
            }

            if is_ctrl_b {
                // Arm the escape state. This key is consumed here and not forwarded.
                self.ssh_escape_pending = true;
                return Ok(ReadResult::Continue);
            }

            // Forward every other key to the remote shell via key_event_to_bytes.
            let bytes = crate::tui::ssh_tab::key_event_to_bytes(key);
            if !bytes.is_empty() {
                if let Some(ref ssh) = self.active_tab().ssh {
                    ssh.send_bytes(&bytes);
                }
                return Ok(ReadResult::Continue);
            }
            // Unrecognised key (empty bytes): fall through to normal handling
            // so things like Ctrl+T (new tab) still work even in SSH mode.
        }

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
        if key.modifiers.contains(KeyModifiers::ALT)
            && let KeyCode::Char(ch) = key.code
                && let Some(n) = ch.to_digit(10)
                    && n >= 1 {
                        self.switch_tab((n as usize).saturating_sub(1));
                        return Ok(ReadResult::Continue);
                    }

        match key.code {
            // F2 / F3: terminal-agnostic tab navigation. Apple Terminal and
            // most others deliver these reliably, unlike Ctrl+arrow or Ctrl+digit.
            KeyCode::F(2) => {
                self.prev_tab();
                return Ok(ReadResult::Continue);
            }
            KeyCode::F(3) => {
                self.next_tab();
                return Ok(ReadResult::Continue);
            }
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
            KeyCode::End => {
                // Bug B fix: if in scrollback historical view, End returns to
                // the live bottom.  Otherwise fall through to move input cursor.
                if !self.active_tab().scrollback_state.is_live() {
                    self.scroll_to_live();
                } else {
                    self.cursor_end();
                }
            }
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
            KeyCode::Right | KeyCode::Char(']') => {
                self.next_tab();
            }
            KeyCode::Left | KeyCode::Char('[') => {
                self.prev_tab();
            }
            KeyCode::Char('n' | 'N') if self.active_tab().input.is_empty() => {
                self.next_tab();
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() && ch != '0' => {
                let n = ch as usize - '0' as usize;
                self.switch_tab(n.saturating_sub(1));
            }
            KeyCode::Char('o' | 'O') => {
                // Priority: if there's a ToolCall entry in the active log, toggle
                // its expanded flag (Ctrl+O expand/collapse the latest tool card).
                // If no ToolCall exists, fall through to focus_mode toggle.
                let mut toggled_tool = false;
                {
                    let tab = self.active_tab_mut();
                    for entry in tab.log.iter_mut().rev() {
                        if let super::state::LogEntry::ToolCall { expanded, .. } = entry {
                            *expanded = !*expanded;
                            toggled_tool = true;
                            break;
                        }
                    }
                }
                if !toggled_tool {
                    self.focus_mode = !self.focus_mode;
                    self.push_system(if self.focus_mode {
                        "Focus view enabled (Ctrl+O to toggle)".to_string()
                    } else {
                        "Focus view disabled".to_string()
                    });
                }
            }
            KeyCode::Char('c' | 'C') => {
                if self.active_tab().input.is_empty() {
                    if let Some(first) = self.ctrl_c_empty_at
                        && first.elapsed() <= Duration::from_secs(1) {
                            return Ok(ReadResult::Exit);
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

        // Determine which item is selected, skipping header rows and
        // free-text placeholder rows (free-text placeholders show a hint but
        // are not inserted verbatim).
        let selected_item = {
            let c = &self.active_tab().completion;
            let item = &c.matches[c.selected];
            if item.is_header || item.is_free_text {
                return; // Nothing to insert for headers or free-text hints.
            }
            item.insert.clone()
        };

        // Build the new input by replacing the partial token that the user was
        // typing with the selected completion text.
        // Strategy: strip off everything after the last space (or the entire
        // input if no space yet), then append the completion.
        let input_clone = self.active_tab().input.clone();
        let base = if let Some(last_space) = input_clone.rfind(' ') {
            &input_clone[..=last_space]
        } else {
            // No space yet — the user is still typing the root command.
            ""
        };
        // Candidate new input (without trailing space added yet).
        let candidate = format!("{base}{selected_item}");

        // Probe whether the accepted token leads to further completions.
        // If so, add a trailing space to prime the next completion level.
        let new_input = if has_further_completions(&candidate) {
            format!("{candidate} ")
        } else {
            candidate
        };

        let new_len = new_input.len();
        let completion = update_completions(&new_input);
        let tab = self.active_tab_mut();
        tab.input = new_input;
        tab.cursor = new_len;
        tab.completion = completion;
    }

    pub(super) fn completion_up(&mut self) {
        let tab = self.active_tab_mut();
        if !tab.completion.visible || tab.completion.matches.is_empty() {
            return;
        }
        let len = tab.completion.matches.len();
        // Step backwards, skipping header rows.
        let mut next = if tab.completion.selected > 0 {
            tab.completion.selected - 1
        } else {
            len - 1
        };
        // Wrap until we land on a non-header row (up to len iterations).
        for _ in 0..len {
            if !tab.completion.matches[next].is_header {
                break;
            }
            next = if next > 0 { next - 1 } else { len - 1 };
        }
        tab.completion.selected = next;
    }

    pub(super) fn completion_down(&mut self) {
        let tab = self.active_tab_mut();
        if !tab.completion.visible || tab.completion.matches.is_empty() {
            return;
        }
        let len = tab.completion.matches.len();
        let mut next = if tab.completion.selected + 1 < len {
            tab.completion.selected + 1
        } else {
            0
        };
        for _ in 0..len {
            if !tab.completion.matches[next].is_header {
                break;
            }
            next = if next + 1 < len { next + 1 } else { 0 };
        }
        tab.completion.selected = next;
    }

    pub(super) fn refresh_completion(&mut self) {
        let input = self.active_tab().input.clone();
        self.active_tab_mut().completion = update_completions(&input);
    }
}
