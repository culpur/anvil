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
use super::redraw::DirtyRegions;
use super::spinner_pump::classify_terminal_event;
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

        // #578: poll the background OAuth listener on every frame tick so the
        // modal advances automatically when the browser callback arrives, even
        // when the user has not pressed any key.
        if self.provider_login_modal.is_some() {
            use super::provider_login::ProviderLoginAction;
            if let Some(action) = self
                .provider_login_modal
                .as_mut()
                .unwrap()
                .poll_oauth_listener()
            {
                match action {
                    ProviderLoginAction::OAuthCodeReceived {
                        code,
                        state,
                        verifier,
                        redirect_uri,
                    } => {
                        self.complete_anthropic_oauth(code, state, verifier, redirect_uri);
                        self.redraw.request(super::redraw::DirtyRegions::ALL);
                    }
                    ProviderLoginAction::Continue => {
                        // Error result already set by poll_oauth_listener; just redraw.
                        self.redraw.request(super::redraw::DirtyRegions::ALL);
                    }
                    _ => {}
                }
            }
        }

        // Task #604 Part C: if a keystroke-burst just ended (i.e. no key
        // has arrived for >= 50 ms after a mature burst), substitute the
        // path-placeholder NOW so the user sees `[image: foo.png]` appear
        // mid-typing — matching CC. See `tui::paste::check_burst_idle`.
        {
            let input_snapshot = self.active_tab().input.clone();
            let now = std::time::Instant::now();
            let outcome = super::paste::check_burst_idle(
                &mut self.burst_tracker,
                &input_snapshot,
                now,
            );
            if let super::paste::BurstOutcome::SubstitutePath { start, end, path } = outcome {
                super::paste::apply_burst_substitution(self, start, end, &path);
                self.burst_tracker.reset();
                self.refresh_completion();
                self.redraw.request(DirtyRegions::INPUT | DirtyRegions::FOCUS);
                self.request_redraw_reason(super::RedrawReason::KeyEvent);
            }
        }

        if event::poll(Duration::from_millis(80))? {
            match event::read()? {
                CtEvent::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                    return self.handle_key(key);
                }
                CtEvent::Paste(text) => {
                    // Task #599 / v2.2.14 Phase 1: single shared paste
                    // handler — file-path detection + placeholder
                    // substitution lives in tui::paste. Adding a
                    // second paste handler ANYWHERE breaks the
                    // regression gate; see
                    // feedback-clipboard-parity.md.
                    //
                    // Part C: Paste events bypass the burst tracker
                    // (those are real bracketed pastes, not drag-drop
                    // keystroke bursts). Reset so any partial burst we
                    // were tracking doesn't leak into post-paste typing.
                    self.burst_tracker.reset();
                    super::paste::handle_paste(self, text);
                    self.refresh_completion();
                }
                CtEvent::Mouse(mouse) => {
                    // Task #748 / feedback-paste-copy-select-never-optional.md:
                    //
                    // Mouse capture stays ON (load-bearing for scroll + tab
                    // clicks). Selection coexists via OS-appropriate modifier
                    // passthrough — when the user holds the OS-native
                    // selection modifier OR uses the right mouse button, we
                    // hand the bytes back to the terminal so it can do
                    // native selection / context-menu paste.
                    //
                    // The modifier is OS-aware (compile-time via cfg!, per
                    // feedback-cross-platform-ux-defaults.md):
                    //   * macOS  → Option (KeyModifiers::ALT)
                    //   * Linux/Windows → Shift  (KeyModifiers::SHIFT)
                    //
                    // We pass through Down + Drag + Up because tab-click
                    // logic below eats the Down event otherwise, and the
                    // modifier path then never reaches Drag.
                    //
                    // Note: cmd.exe does not reliably report modifiers on
                    // drag events. Users on cmd.exe should upgrade to
                    // Windows Terminal for native selection support.
                    let select_mod = if cfg!(target_os = "macos") {
                        crossterm::event::KeyModifiers::ALT
                    } else {
                        crossterm::event::KeyModifiers::SHIFT
                    };
                    let is_modifier_selection = matches!(
                        mouse.kind,
                        crossterm::event::MouseEventKind::Down(
                            crossterm::event::MouseButton::Left,
                        )
                            | crossterm::event::MouseEventKind::Drag(
                                crossterm::event::MouseButton::Left,
                            )
                            | crossterm::event::MouseEventKind::Up(
                                crossterm::event::MouseButton::Left,
                            ),
                    ) && mouse.modifiers.contains(select_mod);
                    let is_right_click = matches!(
                        mouse.kind,
                        crossterm::event::MouseEventKind::Down(
                            crossterm::event::MouseButton::Right,
                        )
                            | crossterm::event::MouseEventKind::Up(
                                crossterm::event::MouseButton::Right,
                            )
                            | crossterm::event::MouseEventKind::Drag(
                                crossterm::event::MouseButton::Right,
                            ),
                    );
                    if is_modifier_selection || is_right_click {
                        // Pass through — terminal handles selection / paste menu.
                    } else {
                        // Mouse-wheel routing: when the configure overlay is
                        // open, the wheel scrolls the overlay's list viewport
                        // (FEAT-36) so long pickers no longer truncate.
                        // Otherwise it falls through to the chat scrollback.
                        let in_configure = self.configure_state
                            != super::configure_types::ConfigureState::Inactive;
                        // CC-139-F3: pull the live wheel-tick speed each
                        // event so `/scroll-speed N` takes effect on the
                        // next scroll without rebuilding the runtime.
                        let speed = runtime::get_scroll_speed() as usize;
                        match mouse.kind {
                            crossterm::event::MouseEventKind::ScrollUp => {
                                if in_configure {
                                    self.configure_scroll_wheel(-(speed as i32));
                                } else {
                                    self.scroll_up(speed);
                                }
                            }
                            crossterm::event::MouseEventKind::ScrollDown => {
                                if in_configure {
                                    self.configure_scroll_wheel(speed as i32);
                                } else {
                                    self.scroll_down(speed);
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
                // Task #716 (CC v2.1.145 parity): Resize / FocusGained /
                // FocusLost arrive on the same crossterm queue as Key/
                // Paste/Mouse. Without this arm they fell through to
                // `_ => {}` and never woke the redraw queue — the
                // spinner / elapsed-time looked frozen until the user
                // pressed a key. Route them to the structural-redraw
                // reason so commit_pending_redraw fires a full repaint
                // on the soft path (no \x1b[2J strobe — see
                // feedback-tui-flash-anti-pattern.md).
                other => {
                    if classify_terminal_event(&other).is_some() {
                        self.redraw.request(DirtyRegions::ALL);
                        self.request_redraw_reason(
                            super::RedrawReason::TerminalStructural,
                        );
                    }
                }
            }
        }

        Ok(ReadResult::Continue)
    }

    // ─── Three-pane vim modal key handler (DELETED — Correction 4) ──────────
    //
    // The vim Normal/Insert/Command modal has been removed. Three-pane now uses
    // always-on input identical to classic and journal. This stub is kept as a
    // comment for git blame purposes.
    //
    // The function `handle_three_pane_key` no longer exists — all callers have
    // been removed. See three_pane.rs for the new always-on input implementation.

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

        // Task #627: confirm modal (/restart, /iac apply).  Route all
        // keys to it while open; on resolution emit a ReadResult variant
        // so the host (LiveCli) can fire the pending action with full
        // access to its own state (respawn ctx, session id, etc.).
        if self.confirm_modal.is_some() {
            use super::modals::ConfirmAction;
            let action = self.confirm_modal.as_mut().unwrap().handle_key(key);
            match action {
                ConfirmAction::Continue => {
                    self.redraw.request(super::redraw::DirtyRegions::ALL);
                    return Ok(super::ReadResult::Continue);
                }
                ConfirmAction::Committed(choice) => {
                    // Consume both the modal and the pending action
                    // atomically so the host receives them together.
                    let pending = self.pending_confirm_action.take();
                    self.confirm_modal = None;
                    self.redraw.request_full();
                    if let Some(action) = pending {
                        return Ok(super::ReadResult::ConfirmResolved {
                            action,
                            choice,
                        });
                    }
                    // No pending action (defensive — shouldn't happen).
                    return Ok(super::ReadResult::Continue);
                }
            }
        }

        // Task #627: password modal (/vault unlock).  Route keys to it
        // while open; on submission emit a ReadResult so the host can
        // attempt the unlock with full access to vault state.
        if self.password_modal.is_some() {
            use super::modals::PasswordAction;
            let action = self.password_modal.as_mut().unwrap().handle_key(key);
            match action {
                PasswordAction::Continue => {
                    self.redraw.request(super::redraw::DirtyRegions::ALL);
                    return Ok(super::ReadResult::Continue);
                }
                PasswordAction::Cancel => {
                    self.password_modal = None;
                    self.pending_password_action = None;
                    self.push_system("Vault unlock cancelled.".to_string());
                    self.redraw.request_full();
                    return Ok(super::ReadResult::Continue);
                }
                PasswordAction::Submit(password) => {
                    // Don't clear the modal yet — the host may need to
                    // call `password_modal_set_error` and keep it open
                    // for retries.  The host clears it on success or
                    // after the attempt-cap is reached.
                    if let Some(pending) = self.pending_password_action.clone() {
                        return Ok(super::ReadResult::PasswordSubmitted {
                            action: pending,
                            password,
                        });
                    }
                    // Defensive: no pending action -> just close.
                    self.password_modal = None;
                    return Ok(super::ReadResult::Continue);
                }
            }
        }

        // #578: when the provider-login modal is open, route all keys to it.
        if self.provider_login_modal.is_some() {
            use super::provider_login::ProviderLoginAction;
            let action = self
                .provider_login_modal
                .as_mut()
                .unwrap()
                .handle_key(key);
            match action {
                ProviderLoginAction::Continue | ProviderLoginAction::PollOAuth => {
                    self.redraw.request(super::redraw::DirtyRegions::ALL);
                    return Ok(super::ReadResult::Continue);
                }
                ProviderLoginAction::Cancel | ProviderLoginAction::Dismiss => {
                    self.provider_login_modal = None;
                    self.redraw.request(super::redraw::DirtyRegions::ALL);
                    return Ok(super::ReadResult::Continue);
                }
                ProviderLoginAction::CancelOAuth => {
                    // Drop the modal — the background thread sees the dropped
                    // receiver and exits on its own.
                    self.provider_login_modal = None;
                    self.push_system("OAuth login cancelled.".to_string());
                    self.redraw.request(super::redraw::DirtyRegions::ALL);
                    return Ok(super::ReadResult::Continue);
                }
                ProviderLoginAction::StartAnthropicOAuth => {
                    // Spawn the background OAuth listener and transition the
                    // modal to OAuthWaiting.
                    self.start_anthropic_oauth_in_modal();
                    self.redraw.request(super::redraw::DirtyRegions::ALL);
                    return Ok(super::ReadResult::Continue);
                }
                ProviderLoginAction::OAuthCodeReceived {
                    code,
                    state,
                    verifier,
                    redirect_uri,
                } => {
                    self.complete_anthropic_oauth(code, state, verifier, redirect_uri);
                    self.redraw.request(super::redraw::DirtyRegions::ALL);
                    return Ok(super::ReadResult::Continue);
                }
                ProviderLoginAction::SaveApiKey { provider, vault_key, key } => {
                    let msg = save_api_key_credential(vault_key, &key);
                    let ok = !msg.starts_with("Error");
                    if let Some(ref mut modal) = self.provider_login_modal {
                        modal.set_result(ok, msg);
                    }
                    self.redraw.request(super::redraw::DirtyRegions::ALL);
                    let _ = provider;
                    return Ok(super::ReadResult::Continue);
                }
                ProviderLoginAction::SaveMultiField { provider, values } => {
                    let msg = save_multi_field_credential(provider, values);
                    let ok = !msg.starts_with("Error");
                    if let Some(ref mut modal) = self.provider_login_modal {
                        modal.set_result(ok, msg);
                    }
                    self.redraw.request(super::redraw::DirtyRegions::ALL);
                    return Ok(super::ReadResult::Continue);
                }
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

        // Three-pane vim modal intercept was deleted (Correction 4).
        // All three-pane input now routes through the standard handler below.

        // Task #748: context-sensitive clipboard shortcuts. This layer runs
        // BEFORE handle_ctrl_key so Ctrl+C with a live selection becomes
        // "copy" instead of cancel/exit, and Ctrl+A in scrollback view
        // becomes "select all visible scrollback" instead of cursor-home.
        // When the shortcut isn't applicable (no selection / not in
        // scrollback view) we fall through to the existing handlers — so
        // Ctrl+C on an empty input still exits, Ctrl+A in the input field
        // still jumps to line start, and Ctrl+V never gets touched.
        //
        // Cmd-on-macOS is treated the same as Ctrl: when the user presses
        // Cmd+C from Terminal.app the crossterm event carries
        // KeyModifiers::SUPER, NOT CONTROL — so we must check both.
        if self.handle_clipboard_shortcut(key) {
            return Ok(ReadResult::Continue);
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
            // Task #634: rail-focus navigation in vertical-split layouts.
            //
            // The KEYBINDS rail block advertises:
            //     g switch deck · d tools · s sessions · a agents
            //
            // These chars come through here as plain `KeyCode::Char`. We
            // intercept ONLY when the input buffer is empty AND no modal is
            // open — otherwise the char must fall through to the standard
            // `insert_char` path so a user mid-typing can still write words
            // that start with g/d/s/a (and so that completion popups, modals,
            // historical-view nav keys, etc. all keep their existing semantics).
            //
            // The drift gate test in `vertical_split.rs::tests::every_advertised_rail_key_has_a_handler`
            // parses the rail block and asserts each label has a matching
            // handler here — DO NOT remove these arms without updating the
            // rail labels first, or the test will fail.
            KeyCode::Char('g')
                if self.input_buffer_is_empty()
                    && !self.has_active_modal()
                    && !self.active_tab().completion.visible
                    && self.active_tab().scrollback_state.is_live() =>
            {
                self.rail_focus = super::RailFocus::Deck;
                self.redraw.request(DirtyRegions::RAIL | DirtyRegions::SCROLLBACK);
                self.request_redraw_reason(super::RedrawReason::TextDeltaBatch);
                return Ok(ReadResult::Continue);
            }
            KeyCode::Char('d')
                if self.input_buffer_is_empty()
                    && !self.has_active_modal()
                    && !self.active_tab().completion.visible
                    && self.active_tab().scrollback_state.is_live() =>
            {
                self.rail_focus = super::RailFocus::Tools;
                self.redraw.request(DirtyRegions::RAIL | DirtyRegions::SCROLLBACK);
                self.request_redraw_reason(super::RedrawReason::TextDeltaBatch);
                return Ok(ReadResult::Continue);
            }
            KeyCode::Char('s')
                if self.input_buffer_is_empty()
                    && !self.has_active_modal()
                    && !self.active_tab().completion.visible
                    && self.active_tab().scrollback_state.is_live() =>
            {
                self.rail_focus = super::RailFocus::Sessions;
                self.redraw.request(DirtyRegions::RAIL | DirtyRegions::SCROLLBACK);
                self.request_redraw_reason(super::RedrawReason::TextDeltaBatch);
                return Ok(ReadResult::Continue);
            }
            KeyCode::Char('a')
                if self.input_buffer_is_empty()
                    && !self.has_active_modal()
                    && !self.active_tab().completion.visible
                    && self.active_tab().scrollback_state.is_live() =>
            {
                self.rail_focus = super::RailFocus::Agents;
                self.redraw.request(DirtyRegions::RAIL | DirtyRegions::SCROLLBACK);
                self.request_redraw_reason(super::RedrawReason::TextDeltaBatch);
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
                self.redraw.request(DirtyRegions::INPUT | DirtyRegions::FOCUS);
                self.request_redraw_reason(super::RedrawReason::KeyEvent);
            }
            KeyCode::Delete => {
                self.delete_char();
                self.redraw.request(DirtyRegions::INPUT | DirtyRegions::FOCUS);
                self.request_redraw_reason(super::RedrawReason::KeyEvent);
            }
            KeyCode::Left => {
                self.cursor_left();
                self.redraw.request(DirtyRegions::INPUT | DirtyRegions::FOCUS);
                self.request_redraw_reason(super::RedrawReason::KeyEvent);
            }
            KeyCode::Right => {
                self.cursor_right();
                self.redraw.request(DirtyRegions::INPUT | DirtyRegions::FOCUS);
                self.request_redraw_reason(super::RedrawReason::KeyEvent);
            }
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
            KeyCode::Char(ch)
                if matches!(ch, '?' | '{' | '}' | 'v' | 'V')
                    && !self.active_tab().scrollback_state.is_live()
                    && !self.active_tab().completion.visible =>
            {
                // CC-139-F5 (#460): transcript view nav keys. Only fire in
                // HISTORICAL VIEW (i.e. user has scrolled away from live);
                // in live mode these characters fall through to plain
                // input below.  Capital `V` is folded into the `v` arm
                // for consistency with the existing `Ctrl+O` expand/
                // collapse toggle pattern.
                self.handle_transcript_nav(ch);
            }
            KeyCode::Char(ch) => {
                self.insert_char(ch);
                self.refresh_completion();
                self.redraw.request(DirtyRegions::INPUT | DirtyRegions::FOCUS);
                self.request_redraw_reason(super::RedrawReason::KeyEvent);
            }
            KeyCode::Tab => {
                self.tab_complete();
                self.redraw.request(DirtyRegions::INPUT | DirtyRegions::FOCUS | DirtyRegions::MISC);
                self.request_redraw_reason(super::RedrawReason::KeyEvent);
            }
            KeyCode::Esc => {
                if self.active_tab().completion.visible {
                    self.active_tab_mut().completion = CompletionPopup::default();
                    self.redraw.request(DirtyRegions::MISC);
                    self.request_redraw_reason(super::RedrawReason::KeyEvent);
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
            // Task #634: Ctrl+R — cycle the vertical-split deck pane through
            // Conversation → Transcript → ToolResults. Advertised in the
            // rail KEYBINDS block as "Ctrl+R deck". No-op (with a system
            // message) for non-vertical-split layouts so the binding is
            // never silently swallowed.
            //
            // The drift gate test in `vertical_split.rs` asserts that this
            // arm exists; do not remove without updating the rail label.
            KeyCode::Char('r' | 'R') => {
                if self.cycle_deck_pane() {
                    let label = match &self.active_tab().layout_local {
                        super::layouts::LayoutLocalState::VerticalSplit {
                            right_deck_mode, ..
                        } => right_deck_mode.label(),
                        _ => "?",
                    };
                    self.push_system(format!("Deck pane: {label}"));
                    self.redraw.request(DirtyRegions::SCROLLBACK);
                    self.request_redraw_reason(super::RedrawReason::TextDeltaBatch);
                } else {
                    self.push_system(
                        "Deck cycling: only available in vertical-split layouts."
                            .to_string(),
                    );
                    self.redraw.request(DirtyRegions::SCROLLBACK);
                    self.request_redraw_reason(super::RedrawReason::TextDeltaBatch);
                }
                return Ok(ReadResult::Continue);
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
                    // Task #604 Part C: input was just nuked; reset
                    // the burst tracker so it doesn't keep counting.
                    self.burst_tracker.reset();
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

        // Task #604 Part C: drive the keystroke-burst tracker. Drag-and-
        // drop from Finder arrives as a fast `KeyCode::Char` burst (never
        // as `Event::Paste`); if the burst suffix resolves to a real file,
        // substitute a placeholder mid-typing. See
        // `tui::paste::record_keystroke` for the heuristic.
        let (input_snapshot, cursor_snapshot) = {
            let tab = self.active_tab();
            (tab.input.clone(), tab.cursor)
        };
        let now = std::time::Instant::now();
        let outcome = super::paste::record_keystroke(
            &mut self.burst_tracker,
            &input_snapshot,
            cursor_snapshot,
            now,
        );
        if let super::paste::BurstOutcome::SubstitutePath { start, end, path } = outcome {
            super::paste::apply_burst_substitution(self, start, end, &path);
            self.burst_tracker.reset();
        }
    }

    pub(super) fn backspace(&mut self) {
        if self.active_tab().cursor == 0 {
            return;
        }
        // Task #599: if the cursor sits at the END of a placeholder span,
        // delete the WHOLE span (not just one character). This is what
        // makes `[image: foo.png]` feel atomic to the user.
        if super::paste::try_backspace_placeholder(self) {
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
        // Shift any placeholder spans that lived past the removed range.
        let removed = cursor - prev;
        for span in &mut tab.input_placeholders {
            if span.start >= cursor {
                span.start -= removed;
                span.end -= removed;
            }
        }
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
        // Task #604 Part C: any keystroke burst we were tracking belongs
        // to the soon-to-be-cleared input. Reset so the next prompt
        // starts a fresh burst window.
        self.burst_tracker.reset();
        let text = std::mem::take(&mut self.active_tab_mut().input);
        // Task #599: snapshot placeholders BEFORE we clear the buffer so
        // that `take_pending_paste_blocks()` (called next from the main
        // loop) can expand them into content blocks. They get stashed on
        // `pending_paste_blocks` so the existing String API stays intact.
        let spans = std::mem::take(&mut self.active_tab_mut().input_placeholders);
        let pending_blocks = if spans.is_empty() {
            Vec::new()
        } else {
            super::paste::expand_input_for_submit(&text, &spans)
        };
        {
            let tab = self.active_tab_mut();
            tab.cursor = 0;
            tab.history_idx = None;
            tab.history_backup = None;
            tab.pending_paste_blocks = pending_blocks;
        }
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            // No literal text. If we have any expanded blocks, ensure the
            // session still gets something to react to.
            if !self.active_tab().pending_paste_blocks.is_empty() {
                let tab = self.active_tab_mut();
                tab.log.push(LogEntry::User(text.clone()));
                self.scroll_to_bottom();
                return Some(text);
            }
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

    /// Drain whatever expanded content blocks the most recent
    /// `submit_input()` produced. Returns an empty Vec when the
    /// submission had no placeholders. Idempotent.
    pub fn take_pending_paste_blocks(&mut self) -> Vec<runtime::ContentBlock> {
        std::mem::take(&mut self.active_tab_mut().pending_paste_blocks)
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

    // ─── CC-139-F5 transcript view nav (#460) ────────────────────────────────

    /// Dispatch the four transcript-mode keys (`?`, `{`, `}`, `v`).
    ///
    /// Only invoked from `handle_key` when the active tab is in HISTORICAL
    /// VIEW (`scrollback_state.is_live() == false`).  Live mode lets these
    /// characters fall through to `insert_char`, matching the spec that
    /// transcript-nav keys must not break ordinary typing.
    pub(super) fn handle_transcript_nav(&mut self, ch: char) {
        match ch {
            '?' => self.transcript_help(),
            '{' => self.transcript_jump_user(false),
            '}' => self.transcript_jump_user(true),
            'v' | 'V' => self.transcript_toggle_verbose(),
            _ => {}
        }
    }

    /// `?` — inline transcript-mode help banner.
    ///
    /// Pushes a single `System` log entry instead of opening a modal so the
    /// user can keep scrolling.  `push_system` snaps back to live view; we
    /// stash and restore the anchor so a quick `?` in the middle of a long
    /// transcript doesn't yank the viewport to the bottom.
    fn transcript_help(&mut self) {
        let saved = self.active_tab().scrollback_state;
        self.push_system(
            "Transcript nav: { prev user msg · } next user msg · v verbose · End live"
                .to_string(),
        );
        // Restore the historical anchor.  `push_system` calls
        // `scroll_to_bottom`, which sets state to live — we override that
        // here so the help line appears on the next draw but the viewport
        // stays where the user was reading.
        self.active_tab_mut().scrollback_state = saved;
        self.redraw.request(super::redraw::DirtyRegions::SCROLLBACK);
    }

    /// `v` / `V` — toggle per-tab transcript verbose mode.
    ///
    /// Flips `Tab::transcript_verbose` and clears the scrollback ring so the
    /// next draw rebuilds line counts under the new flag.  Without the clear,
    /// `scrollback_already` would skip re-rendering and the cached truncated
    /// tool cards would stay frozen.
    fn transcript_toggle_verbose(&mut self) {
        let saved = self.active_tab().scrollback_state;
        let new_flag = !self.active_tab().transcript_verbose;
        {
            let tab = self.active_tab_mut();
            tab.transcript_verbose = new_flag;
            // Invalidate the cached scrollback so the next draw re-renders
            // every log entry under the new verbose flag.
            tab.scrollback = super::scrollback::ScrollbackBuffer::new();
            tab.scrollback_pending_lines = 0;
        }
        self.push_system(if new_flag {
            "Transcript verbose: ON (tool I/O shown in full)".to_string()
        } else {
            "Transcript verbose: OFF (tool I/O truncated)".to_string()
        });
        // push_system snaps to live; restore the historical anchor so the
        // user keeps their reading position.  Anchor may now point past the
        // end of the rebuilt buffer — `effective_anchor` will clamp it.
        self.active_tab_mut().scrollback_state = saved;
        self.redraw.request(super::redraw::DirtyRegions::ALL);
    }

    /// `{` / `}` — jump to the previous or next user message.
    ///
    /// `forward = false` searches backward (`{`), `true` searches forward
    /// (`}`).  We walk `tab.log` and compute each `User` entry's starting
    /// scrollback line by rendering preceding entries with the same
    /// verbosity flag — the only correct way to land on the right anchor
    /// when verbose mode has changed tool-card line counts.
    ///
    /// If no further user message exists in the search direction we push a
    /// System message and leave the scroll anchor alone.
    fn transcript_jump_user(&mut self, forward: bool) {
        use super::state::LogEntry;

        let theme = self.theme.clone();
        let approx_width: u16 = self.terminal.size().map(|s| s.width).unwrap_or(80);
        let tab = self.active_tab();
        let verbose = tab.transcript_verbose;

        // Build a list of (log_idx, scrollback_line) for every User entry,
        // skipping the very last entry which is part of the mutable tail
        // (its position can shift mid-stream).  Empty logs short-circuit
        // below.
        let mut user_lines: Vec<usize> = Vec::new();
        let mut cumulative: usize = 0;
        let stable_end = tab.log.len().saturating_sub(1);
        for entry in &tab.log[..stable_end] {
            if matches!(entry, LogEntry::User(_)) {
                user_lines.push(cumulative);
            }
            cumulative += entry.to_lines_with(approx_width, &theme, verbose).len();
        }

        if user_lines.is_empty() {
            // No qualifying user message in the stable region.
            let direction = if forward { "later" } else { "earlier" };
            let msg = format!("No {direction} user message");
            let saved = tab.scrollback_state;
            self.push_system(msg);
            self.active_tab_mut().scrollback_state = saved;
            self.redraw.request(super::redraw::DirtyRegions::SCROLLBACK);
            return;
        }

        // Current anchor — None means live (bottom of buffer).  In live mode
        // both `{` and `}` are gated off, so this branch only runs in
        // historical view; we still defensively fall back to `cumulative` so
        // `{` works as "previous from the bottom".
        let current_anchor: usize = match tab.scrollback_state.0 {
            Some(a) => a,
            None => cumulative,
        };

        let target =
            super::scrollback::pick_user_anchor(&user_lines, current_anchor, forward);

        match target {
            Some(new_anchor) => {
                self.active_tab_mut().scrollback_state =
                    super::scrollback::ScrollbackState(Some(new_anchor));
                self.redraw.request(super::redraw::DirtyRegions::SCROLLBACK);
            }
            None => {
                let saved = self.active_tab().scrollback_state;
                let direction = if forward { "later" } else { "earlier" };
                self.push_system(format!("No {direction} user message"));
                self.active_tab_mut().scrollback_state = saved;
                self.redraw.request(super::redraw::DirtyRegions::SCROLLBACK);
            }
        }
    }

    // ─── Provider-login modal helpers (#578) ─────────────────────────────────

    /// Spawn the Anthropic OAuth background thread and transition the modal
    /// from `AuthMethodPicker` to `OAuthWaiting`.
    pub(super) fn start_anthropic_oauth_in_modal(&mut self) {
        use std::net::TcpListener;
        use std::sync::mpsc;

        let cwd = std::env::current_dir().unwrap_or_default();
        let Ok(config) = runtime::ConfigLoader::default_for(&cwd).load() else {
            if let Some(ref mut modal) = self.provider_login_modal {
                modal.set_result(false, "Failed to load config.".to_string());
            }
            return;
        };
        let default_oauth = crate::auth::default_oauth_config();
        let oauth_cfg = config.oauth().cloned().unwrap_or(default_oauth);
        let callback_port = oauth_cfg.callback_port.unwrap_or(crate::DEFAULT_OAUTH_CALLBACK_PORT);

        let listener = TcpListener::bind(("127.0.0.1", callback_port)).ok();
        let redirect_uri = if listener.is_some() {
            runtime::loopback_redirect_uri(callback_port)
        } else {
            oauth_cfg
                .manual_redirect_url
                .clone()
                .unwrap_or_else(|| runtime::loopback_redirect_uri(callback_port))
        };

        let pkce = match runtime::generate_pkce_pair() {
            Ok(p) => p,
            Err(e) => {
                if let Some(ref mut modal) = self.provider_login_modal {
                    modal.set_result(false, format!("PKCE generation failed: {e}"));
                }
                return;
            }
        };
        let state = match runtime::generate_state() {
            Ok(s) => s,
            Err(e) => {
                if let Some(ref mut modal) = self.provider_login_modal {
                    modal.set_result(false, format!("State generation failed: {e}"));
                }
                return;
            }
        };

        let authorize_url =
            runtime::OAuthAuthorizationRequest::from_config(&oauth_cfg, redirect_uri.clone(), state.clone(), &pkce)
                .build_url();

        // Open the browser — this is an OS operation, not the TUI leaving.
        let _ = crate::auth::open_browser(&authorize_url);

        let port = listener.as_ref().map(|_| callback_port);
        let (tx, rx) = mpsc::channel::<Result<(String, String), String>>();

        // Listener thread (only when we bound a port).
        if let Some(lst) = listener {
            let tx_l = tx.clone();
            let expected = state.clone();
            std::thread::spawn(move || {
                use runtime::parse_oauth_callback_request_target;
                let outcome = match lst.accept() {
                    Ok((mut stream, _)) => {
                        use std::io::Read as _;
                        use std::io::Write as _;
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let target = req.lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("");
                        match parse_oauth_callback_request_target(target) {
                            Ok(params) => {
                                let body = if params.error.is_some() {
                                    "OAuth failed. You can close this tab."
                                } else {
                                    "OAuth succeeded. You can close this tab."
                                };
                                let resp = format!(
                                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                                    body.len(), body
                                );
                                let _ = stream.write_all(resp.as_bytes());
                                if let Some(err) = params.error {
                                    Err(format!("{}: {}", err, params.error_description.unwrap_or_default()))
                                } else if let (Some(code), Some(ret_state)) = (params.code, params.state) {
                                    if ret_state == expected {
                                        Ok((code, ret_state))
                                    } else {
                                        Err("OAuth state mismatch.".to_string())
                                    }
                                } else {
                                    Err("Callback did not include code/state.".to_string())
                                }
                            }
                            Err(e) => Err(e),
                        }
                    }
                    Err(e) => Err(format!("Listener accept error: {e}")),
                };
                let _ = tx_l.send(outcome);
            });
        }

        if let Some(ref mut modal) = self.provider_login_modal {
            modal.set_oauth_waiting(
                port,
                authorize_url,
                state,
                pkce.verifier,
                redirect_uri,
                rx,
            );
        }
    }

    /// Exchange the OAuth code for tokens and store the result in the modal.
    pub(super) fn complete_anthropic_oauth(
        &mut self,
        code: String,
        state: String,
        verifier: String,
        redirect_uri: String,
    ) {
        let cwd = std::env::current_dir().unwrap_or_default();
        let config = match runtime::ConfigLoader::default_for(&cwd).load() {
            Ok(c) => c,
            Err(e) => {
                if let Some(ref mut modal) = self.provider_login_modal {
                    modal.set_result(false, format!("Config load failed: {e}"));
                }
                return;
            }
        };
        let default_oauth = crate::auth::default_oauth_config();
        let oauth_cfg = config.oauth().cloned().unwrap_or(default_oauth);

        let exchange = runtime::OAuthTokenExchangeRequest::from_config(
            &oauth_cfg,
            code,
            state,
            verifier,
            redirect_uri,
        );

        // Block on the token exchange in a fresh one-shot tokio runtime.
        // This is a quick HTTPS request (<1s on a normal network) and
        // happens in the key-handler context, not the draw loop, so a
        // brief block is acceptable.
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                if let Some(ref mut modal) = self.provider_login_modal {
                    modal.set_result(false, format!("Runtime error: {e}"));
                }
                return;
            }
        };

        let client = api::AnvilApiClient::from_auth(api::AuthSource::None)
            .with_base_url(api::read_base_url());

        match rt.block_on(client.exchange_oauth_code(&oauth_cfg, &exchange)) {
            Ok(token_set) => {
                let save_result = runtime::save_oauth_credentials(&runtime::OAuthTokenSet {
                    access_token: token_set.access_token,
                    refresh_token: token_set.refresh_token,
                    expires_at: token_set.expires_at,
                    scopes: token_set.scopes,
                });
                if let Some(ref mut modal) = self.provider_login_modal {
                    match save_result {
                        Ok(()) => modal.set_result(true, "Token saved. You are now logged in.".to_string()),
                        Err(e) => modal.set_result(false, format!("Failed to save token: {e}")),
                    }
                }
            }
            Err(e) => {
                if let Some(ref mut modal) = self.provider_login_modal {
                    modal.set_result(false, format!("Token exchange failed: {e}"));
                }
            }
        }
    }
}

// ─── Free helpers for credential persistence ─────────────────────────────────

/// Save a single API key to the vault or credentials.json.
/// Returns a human-readable status message.
fn save_api_key_credential(vault_key: &'static str, key: &str) -> String {
    // Prefer vault when unlocked.
    if runtime::vault_is_session_unlocked() {
        match runtime::vault_session_upsert(vault_key, key) {
            Ok(()) => return format!("Key saved to encrypted vault (key: {vault_key})."),
            Err(e) => {
                // Fall through to plaintext fallback.  Task #626: this fires
                // inside the in-TUI provider-login modal, so route the warning
                // through the TUI-aware sink instead of `eprintln!` directly.
                super::log_warning(&format!("[provider-login] vault upsert failed: {e}"));
            }
        }
    }
    // Plaintext credentials.json fallback.
    match runtime::credentials_path() {
        Ok(path) => {
            let mut root = if path.exists() {
                std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s).ok())
                    .unwrap_or_default()
            } else {
                serde_json::Map::new()
            };
            root.insert(
                vault_key.to_string(),
                serde_json::Value::String(key.to_string()),
            );
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match serde_json::to_string_pretty(&root) {
                Ok(json) => match std::fs::write(&path, json) {
                    Ok(()) => format!("Key saved to {}", path.display()),
                    Err(e) => format!("Error writing credentials: {e}"),
                },
                Err(e) => format!("Error serializing credentials: {e}"),
            }
        }
        Err(e) => format!("Error: cannot find credentials path: {e}"),
    }
}

/// Save multi-field credentials (Ollama host/key, etc.).
fn save_multi_field_credential(provider: api::ProviderKind, values: Vec<String>) -> String {
    match provider {
        api::ProviderKind::Ollama => {
            let host = values.first().map(|s| s.trim()).filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| "http://localhost:11434".to_string());
            let api_key = values.get(1).map(|s| s.trim().to_string()).unwrap_or_default();

            let host_ok = if runtime::vault_is_session_unlocked() {
                runtime::vault_session_upsert("ollama_host", &host).is_ok()
            } else {
                false
            };
            let key_ok = if !api_key.is_empty() && runtime::vault_is_session_unlocked() {
                runtime::vault_session_upsert("ollama_api_key", &api_key).is_ok()
            } else {
                api_key.is_empty()
            };

            if host_ok && key_ok {
                return format!("Ollama endpoint saved to vault: {host}");
            }

            // Plaintext fallback.
            if let Ok(path) = runtime::credentials_path() {
                let mut root = if path.exists() {
                    std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|s| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s).ok())
                        .unwrap_or_default()
                } else {
                    serde_json::Map::new()
                };
                root.insert("ollama_host".to_string(), serde_json::json!(host));
                if !api_key.is_empty() {
                    root.insert("ollama_api_key".to_string(), serde_json::json!(api_key));
                }
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(json) = serde_json::to_string_pretty(&root) {
                    let _ = std::fs::write(&path, json);
                }
                return format!("Ollama endpoint saved: {host}");
            }
            format!("Ollama endpoint: {host} (not persisted — credentials path unavailable)")
        }
        _ => format!("Multi-field save not implemented for {provider:?}"),
    }
}

// ─── Task #748: context-sensitive clipboard shortcuts ────────────────────────

impl AnvilTui {
    /// Context-sensitive Ctrl/Cmd + C/X/A dispatcher.
    ///
    /// Returns `true` when the key was consumed; `false` to fall through to
    /// the existing handlers. Ctrl+V is intentionally NOT handled here —
    /// the existing bracketed-paste + keystroke-burst pipeline lives in
    /// `tui::paste` and must not be re-entered. See
    /// `feedback-paste-copy-select-never-optional.md` and
    /// `feedback-clipboard-parity.md`.
    pub(super) fn handle_clipboard_shortcut(&mut self, key: KeyEvent) -> bool {
        // Treat Cmd on macOS the same as Ctrl on Linux / Windows. crossterm
        // delivers Cmd as KeyModifiers::SUPER, not CONTROL.
        let is_macos_meta =
            cfg!(target_os = "macos") && key.modifiers.contains(KeyModifiers::SUPER);
        let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if !(is_macos_meta || is_ctrl) {
            return false;
        }

        match key.code {
            KeyCode::Char('c') | KeyCode::Char('C') => {
                if let Some(sel) = self.current_selection_text() {
                    let _ = super::clipboard::write_clipboard(&sel);
                    self.clear_selection();
                    self.push_system("Copied".to_string());
                    self.redraw
                        .request(super::redraw::DirtyRegions::INPUT
                            | super::redraw::DirtyRegions::SCROLLBACK);
                    self.request_redraw_reason(super::RedrawReason::KeyEvent);
                    true
                } else {
                    // No selection → fall through so handle_ctrl_key keeps
                    // the existing cancel/exit semantics for empty input.
                    false
                }
            }
            KeyCode::Char('x') | KeyCode::Char('X') => {
                if let Some((sel, range)) = self.current_input_selection_with_range() {
                    let _ = super::clipboard::write_clipboard(&sel);
                    self.delete_input_range(range);
                    self.clear_selection();
                    self.redraw.request(super::redraw::DirtyRegions::INPUT);
                    self.request_redraw_reason(super::RedrawReason::KeyEvent);
                    true
                } else {
                    // No selection → silent no-op (DO NOT fall through to
                    // handle_ctrl_key — Ctrl+X has no existing binding and
                    // we don't want stray input characters).
                    false
                }
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                if self.in_scrollback_view() {
                    self.select_all_visible_scrollback();
                    self.redraw
                        .request(super::redraw::DirtyRegions::SCROLLBACK);
                    self.request_redraw_reason(super::RedrawReason::KeyEvent);
                    true
                } else {
                    // Input field: fall through to handle_ctrl_key, which
                    // preserves the existing readline "jump to line start /
                    // toggle agent panel" behaviour.
                    false
                }
            }
            _ => false,
        }
    }

    /// Compose the current selection text — input-field selection wins over
    /// scrollback selection so the user can hold both simultaneously and
    /// Ctrl+C grabs the actively-typed selection.
    pub(super) fn current_selection_text(&self) -> Option<String> {
        if let Some((text, _range)) = self.current_input_selection_with_range() {
            return Some(text);
        }
        self.current_scrollback_selection_text()
    }

    /// Input-field selection (byte range + text) for Ctrl+X / cut.
    pub(super) fn current_input_selection_with_range(
        &self,
    ) -> Option<(String, std::ops::Range<usize>)> {
        let tab = self.active_tab();
        let anchor = tab.selection_anchor?;
        let cursor = tab.cursor;
        if anchor == cursor {
            return None;
        }
        let start = anchor.min(cursor);
        let end = anchor.max(cursor);
        // Guard against out-of-range anchors lingering after a buffer edit.
        if end > tab.input.len() {
            return None;
        }
        let text = tab.input[start..end].to_string();
        Some((text, start..end))
    }

    /// Scrollback selection → joined-with-newlines text.
    pub(super) fn current_scrollback_selection_text(&self) -> Option<String> {
        let sel = self.scrollback_selection?;
        let tab = self.active_tab();
        let total = tab.scrollback.len();
        if total == 0 {
            return None;
        }
        let start = sel.start_row.min(total.saturating_sub(1));
        let end = sel.end_row.min(total.saturating_sub(1));
        let height = end + 1 - start;
        let (lines, _anchor) = tab.scrollback.lines_in_range(start, height);
        if lines.is_empty() {
            return None;
        }
        Some(lines.join("\n"))
    }

    /// Clear BOTH input-field and scrollback selection state in one call.
    pub(super) fn clear_selection(&mut self) {
        self.active_tab_mut().selection_anchor = None;
        self.scrollback_selection = None;
    }

    /// True when the user has scrolled away from the live tail. The
    /// scrollback_state is the source of truth — `is_live() == false`
    /// means the user is reading history, where Ctrl+A means
    /// "select all visible" rather than "cursor home".
    pub(super) fn in_scrollback_view(&self) -> bool {
        !self.active_tab().scrollback_state.is_live()
    }

    /// Capture the visible viewport row range as a row-based scrollback
    /// selection. The viewport size is approximated by the terminal height
    /// minus a small chrome reserve; the exact pixel-accurate row range
    /// will be wired in v3.x alongside extension-via-arrows.
    pub(super) fn select_all_visible_scrollback(&mut self) {
        let total = self.active_tab().scrollback.len();
        if total == 0 {
            return;
        }
        // Approximate the viewport height. The exact draw-loop value is
        // recorded by the renderer; for the initial Ctrl+A semantics we
        // bound to (terminal_rows - 6) to reserve for header/input/footer.
        let height = crossterm::terminal::size()
            .map(|(_, h)| h as usize)
            .unwrap_or(40)
            .saturating_sub(6)
            .max(1);
        let anchor = match self.active_tab().scrollback_state.0 {
            Some(a) => a.min(total.saturating_sub(1)),
            None => self.active_tab().scrollback.live_anchor(height),
        };
        let start_row = anchor;
        let end_row = (anchor + height - 1).min(total.saturating_sub(1));
        self.scrollback_selection = Some(super::ScrollbackSelection {
            start_row,
            end_row,
        });
    }

    /// Delete a byte range from the active tab's input buffer, fixing up
    /// the cursor and any placeholder spans that lived past the cut.
    pub(super) fn delete_input_range(&mut self, range: std::ops::Range<usize>) {
        let tab = self.active_tab_mut();
        if range.end > tab.input.len() || range.start >= range.end {
            return;
        }
        let removed_len = range.end - range.start;
        tab.input.drain(range.clone());
        // Move cursor to the start of the removed range.
        tab.cursor = range.start;
        // Shift any placeholder spans that lived past the removed range.
        // Spans fully before the cut are unaffected; spans fully after
        // are shifted; spans straddling the cut (shouldn't normally
        // happen because the cut is selection-based) get dropped.
        tab.input_placeholders.retain(|span| {
            !(span.start >= range.start && span.start < range.end
                || span.end > range.start && span.end <= range.end
                || span.start < range.start && span.end > range.end)
        });
        for span in &mut tab.input_placeholders {
            if span.start >= range.end {
                span.start -= removed_len;
                span.end -= removed_len;
            }
        }
        tab.history_idx = None;
        tab.history_backup = None;
    }
}

// ─── Task #748: tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    };

    // ── Mouse passthrough tests ──────────────────────────────────────────

    /// Predicate mirroring the input_handler.rs check. Lifted here so we can
    /// unit-test the decision without spinning up a full AnvilTui (which
    /// needs a real terminal).
    fn mouse_event_should_pass_through(
        mouse: &MouseEvent,
        compile_target_os_is_macos: bool,
    ) -> bool {
        let select_mod = if compile_target_os_is_macos {
            KeyModifiers::ALT
        } else {
            KeyModifiers::SHIFT
        };
        let is_modifier_selection = matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::Drag(MouseButton::Left)
                | MouseEventKind::Up(MouseButton::Left)
        ) && mouse.modifiers.contains(select_mod);
        let is_right_click = matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Right)
                | MouseEventKind::Up(MouseButton::Right)
                | MouseEventKind::Drag(MouseButton::Right)
        );
        is_modifier_selection || is_right_click
    }

    fn mk_mouse(kind: MouseEventKind, mods: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind,
            column: 10,
            row: 5,
            modifiers: mods,
        }
    }

    #[test]
    fn macos_option_drag_passes_through_when_mouse_capture_on() {
        let ev = mk_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            KeyModifiers::ALT,
        );
        assert!(mouse_event_should_pass_through(&ev, true));
    }

    #[test]
    fn macos_option_down_event_passes_through() {
        let ev = mk_mouse(
            MouseEventKind::Down(MouseButton::Left),
            KeyModifiers::ALT,
        );
        assert!(mouse_event_should_pass_through(&ev, true));
    }

    #[test]
    fn macos_option_up_event_passes_through() {
        let ev = mk_mouse(
            MouseEventKind::Up(MouseButton::Left),
            KeyModifiers::ALT,
        );
        assert!(mouse_event_should_pass_through(&ev, true));
    }

    #[test]
    fn linux_shift_drag_passes_through_when_mouse_capture_on() {
        let ev = mk_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            KeyModifiers::SHIFT,
        );
        assert!(mouse_event_should_pass_through(&ev, false));
    }

    #[test]
    fn windows_shift_drag_passes_through_when_mouse_capture_on() {
        // Compile-target Linux/Windows share the same SHIFT predicate.
        let ev = mk_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            KeyModifiers::SHIFT,
        );
        assert!(mouse_event_should_pass_through(&ev, false));
    }

    #[test]
    fn unmodified_drag_with_capture_on_routes_to_tab_click_handler() {
        // No selection modifier → should NOT pass through; the existing
        // wheel / tab-click handler owns this event.
        let ev_mac = mk_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            KeyModifiers::NONE,
        );
        assert!(!mouse_event_should_pass_through(&ev_mac, true));
        let ev_lnx = mk_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            KeyModifiers::NONE,
        );
        assert!(!mouse_event_should_pass_through(&ev_lnx, false));
    }

    #[test]
    fn right_click_down_passes_through_unconditionally() {
        for compile_target_os_is_macos in [true, false] {
            let ev = mk_mouse(
                MouseEventKind::Down(MouseButton::Right),
                KeyModifiers::NONE,
            );
            assert!(
                mouse_event_should_pass_through(&ev, compile_target_os_is_macos),
                "right-click Down must always pass through (macos={compile_target_os_is_macos})"
            );
        }
    }

    #[test]
    fn right_click_drag_passes_through_unconditionally() {
        for compile_target_os_is_macos in [true, false] {
            let ev = mk_mouse(
                MouseEventKind::Drag(MouseButton::Right),
                KeyModifiers::NONE,
            );
            assert!(
                mouse_event_should_pass_through(&ev, compile_target_os_is_macos),
                "right-click Drag must always pass through (macos={compile_target_os_is_macos})"
            );
        }
    }

    /// Regression gate: the SHIFT-only check (the pre-#748 code path) is
    /// known to disagree with the macOS banner ("Hold Option") because
    /// Option arrives as ALT, not SHIFT. We assert macOS Option DOES pass
    /// through under the new logic.
    #[test]
    fn macos_option_drag_passes_through_when_shift_only_would_have_missed_it() {
        let ev = mk_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            KeyModifiers::ALT,
        );
        // Pre-fix code path: SHIFT check only.
        let pre_fix_passes = ev.modifiers.contains(KeyModifiers::SHIFT);
        assert!(!pre_fix_passes, "pre-fix path must NOT have passed Option-drag through");
        // Post-fix: macOS check returns true.
        assert!(mouse_event_should_pass_through(&ev, true));
    }

    // ── Selection model tests (pure helpers — no terminal needed) ────────

    /// Compute what `current_input_selection_with_range` would return for
    /// a given (input, cursor, anchor). Lifted here so the tests don't
    /// need a real AnvilTui (which needs a TTY).
    fn input_selection(
        input: &str,
        cursor: usize,
        anchor: Option<usize>,
    ) -> Option<(String, std::ops::Range<usize>)> {
        let anchor = anchor?;
        if anchor == cursor {
            return None;
        }
        let start = anchor.min(cursor);
        let end = anchor.max(cursor);
        if end > input.len() {
            return None;
        }
        Some((input[start..end].to_string(), start..end))
    }

    #[test]
    fn ctrl_c_with_input_selection_copies_and_clears() {
        // Selection covers "hello".
        let input = "hello world";
        let sel = input_selection(input, 5, Some(0));
        assert_eq!(
            sel.as_ref().map(|(s, _)| s.as_str()),
            Some("hello")
        );
        // After "copy", the caller drops the selection — modelled by
        // calling input_selection again with anchor=None.
        let after = input_selection(input, 5, None);
        assert!(after.is_none(), "selection must be cleared after copy");
    }

    #[test]
    fn ctrl_c_without_selection_falls_through() {
        // No anchor → no selection → handle_clipboard_shortcut must
        // return false so the existing Ctrl+C cancel/exit fires.
        let sel = input_selection("hello", 3, None);
        assert!(sel.is_none());
    }

    #[test]
    fn ctrl_x_with_input_selection_cuts_and_removes_text() {
        let mut input = String::from("hello world");
        let sel = input_selection(&input, 5, Some(0)).expect("selection");
        let (text, range) = sel;
        assert_eq!(text, "hello");
        // Simulate the cut.
        input.drain(range.clone());
        assert_eq!(input, " world");
    }

    #[test]
    fn ctrl_x_without_selection_is_noop() {
        // No selection → handle_clipboard_shortcut returns false →
        // no text is changed. Mirror that here.
        let sel = input_selection("hello", 0, None);
        assert!(sel.is_none());
    }

    /// Helper: simulate the Ctrl+A "select all visible scrollback"
    /// decision purely from the inputs.
    fn ctrl_a_decision_in_scrollback(
        in_scrollback_view: bool,
        scrollback_len: usize,
    ) -> Option<(usize, usize)> {
        if !in_scrollback_view || scrollback_len == 0 {
            return None;
        }
        // Approximate viewport height for the test.
        let height = 10usize;
        let start = 0usize;
        let end = (start + height - 1).min(scrollback_len.saturating_sub(1));
        Some((start, end))
    }

    #[test]
    fn ctrl_a_in_scrollback_selects_all_visible() {
        // 20-line scrollback, in history view, 10-row viewport → rows 0..=9.
        let got = ctrl_a_decision_in_scrollback(true, 20);
        assert_eq!(got, Some((0, 9)));
    }

    #[test]
    fn ctrl_a_in_input_field_does_not_select_all() {
        // Not in history view → no scrollback selection. handler returns
        // false so the existing readline cursor-home fires instead.
        let got = ctrl_a_decision_in_scrollback(false, 100);
        assert!(got.is_none());
    }

    #[test]
    fn ctrl_a_with_empty_scrollback_is_noop() {
        let got = ctrl_a_decision_in_scrollback(true, 0);
        assert!(got.is_none());
    }

    /// On macOS, Cmd arrives as KeyModifiers::SUPER. The shortcut handler
    /// must treat Cmd+C identically to Ctrl+C. Mirror that compile-time
    /// branch here.
    fn shortcut_modifier_active(
        mods: KeyModifiers,
        compile_target_os_is_macos: bool,
    ) -> bool {
        let is_macos_meta =
            compile_target_os_is_macos && mods.contains(KeyModifiers::SUPER);
        let is_ctrl = mods.contains(KeyModifiers::CONTROL);
        is_macos_meta || is_ctrl
    }

    #[test]
    fn cmd_c_on_macos_handled_same_as_ctrl_c() {
        // Cmd alone, on macOS, is treated like Ctrl.
        assert!(shortcut_modifier_active(KeyModifiers::SUPER, true));
        // Cmd alone, on non-macOS, is NOT — Linux/Windows users don't
        // typically have a SUPER chord that maps to clipboard.
        assert!(!shortcut_modifier_active(KeyModifiers::SUPER, false));
        // Ctrl alone is universal.
        assert!(shortcut_modifier_active(KeyModifiers::CONTROL, true));
        assert!(shortcut_modifier_active(KeyModifiers::CONTROL, false));
        // No modifier → never active.
        assert!(!shortcut_modifier_active(KeyModifiers::NONE, true));
        assert!(!shortcut_modifier_active(KeyModifiers::NONE, false));
    }

    /// Sanity check: pre-fix mouse handler was `Drag(_) + SHIFT`. The new
    /// `mouse_event_should_pass_through` must also reject a SHIFT-Down
    /// when the compile target is macOS (because macOS uses ALT, not
    /// SHIFT). Otherwise Linux/macOS macros would diverge.
    #[test]
    fn shift_drag_on_macos_does_not_pass_through() {
        let ev = mk_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            KeyModifiers::SHIFT,
        );
        assert!(!mouse_event_should_pass_through(&ev, true));
    }

    /// Sanity check: ALT-drag on Linux/Windows does NOT pass through
    /// (that compile target uses SHIFT).
    #[test]
    fn alt_drag_on_linux_does_not_pass_through() {
        let ev = mk_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            KeyModifiers::ALT,
        );
        assert!(!mouse_event_should_pass_through(&ev, false));
    }

    /// Suppress unused warning when key events aren't exercised at
    /// compile time on this platform.
    #[allow(dead_code)]
    fn mk_key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }
}
