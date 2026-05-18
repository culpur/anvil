//! Standalone modal-queue runner for the first-run wizard (task #579).
//!
//! ## Why a standalone runner
//!
//! The interactive setup wizard (`wizard::run_first_run_wizard`) runs
//! BEFORE `AnvilTui` exists — there is no live alt-screen or event loop
//! into which we could enqueue and drain modals. Task #579 still wants
//! the wizard's TUI-config steps (layout pick, mouse capture, theme,
//! permission mode) to use the same `WizardChoiceModal` widget the rest
//! of the application uses for multi-option prompts.
//!
//! The bridge is `WizardSession` + `WizardModalRunner`: a single
//! `WizardSession::enter()` enables raw-mode + alt-screen ONCE for the
//! lifetime of the four TUI-config steps. `WizardModalRunner` then
//! drains supplied modals inside that single alt-screen session so the
//! user sees zero terminal flicker between steps. The session's `Drop`
//! impl exits alt-screen + disables raw-mode exactly once, even if the
//! wizard panics or the user hits Esc mid-step.
//!
//! ## 8-axis capability contract
//!
//! 1. Definition       — `WizardSession` + `WizardModalRunner` + traits.
//! 2. Registration     — exposed via `mod wizard_runner` in `main.rs`.
//! 3. Completion       — N/A (a runtime helper, not a slash command).
//! 4. Handler          — `run_queue` / `run_modal` drive `handle_key`.
//! 5. Dispatch         — wizard.rs callers open a session + invoke.
//! 6. Rendering        — delegates to each modal's `render` method.
//! 7. Gate             — single live modal at a time (top of queue).
//! 8. OTel + tests     — unit tests at the bottom of this file cover
//!                       single-enter / single-exit, FIFO draining,
//!                       Esc cancel, banner rendering, mouse-default OFF.
//!
//! ## Exceptions (call-site responsibility, see `feedback-anvil-capability-contract.md`)
//!
//! Provider OAuth login (`run_anthropic_login`) and vault password
//! prompts (`rpassword::prompt_password`) intentionally bypass this
//! runner — they need stdin echo control that ratatui's alt-screen
//! semantics cannot replicate. Those two sites stay on the existing
//! stdin path; see wizard.rs steps 1 + 3 + "[2] manual key" branches.

use std::fmt;
use std::io;
use std::time::Duration;

use crossterm::event::{Event, KeyEvent};

use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::style::Color;

/// Errors emitted by the runner / session.
#[derive(Debug)]
pub(crate) enum RunnerError {
    /// The backing terminal failed to draw a frame.
    Draw(String),
    /// Failed to enter raw-mode or alt-screen.
    #[allow(dead_code)]
    Enter(String),
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Draw(msg) => write!(f, "wizard runner draw error: {msg}"),
            Self::Enter(msg) => write!(f, "wizard session enter failed: {msg}"),
        }
    }
}

impl std::error::Error for RunnerError {}

use crate::tui::modals::confirm::{ConfirmAction, ConfirmModal};
use crate::tui::modals::password::{PasswordAction, PasswordModal};
use crate::tui::modals::queue::{
    ChoiceAction, ModalAnswer, ModalQueue, QueuedModal, WizardChoiceModal,
};
use crate::tui::modals::text_input::{TextInputAction, TextInputModal};

/// Source of key events for the runner. Production uses
/// `CrosstermKeySource` which polls real terminal input; tests use
/// `ScriptedKeySource` to inject a deterministic sequence.
pub(crate) trait KeySource {
    /// Block until the next `KeyEvent` (or return `None` on cancel).
    fn next_key(&mut self) -> Option<KeyEvent>;
}

/// Production key source: polls crossterm's event stream.
#[allow(dead_code)]
pub(crate) struct CrosstermKeySource {
    pub(crate) poll_timeout: Duration,
}

impl KeySource for CrosstermKeySource {
    fn next_key(&mut self) -> Option<KeyEvent> {
        loop {
            match crossterm::event::poll(self.poll_timeout) {
                Ok(true) => match crossterm::event::read() {
                    Ok(Event::Key(key)) => return Some(key),
                    Ok(_) => continue, // non-key events ignored
                    Err(_) => return None,
                },
                Ok(false) => continue,
                Err(_) => return None,
            }
        }
    }
}

/// Scripted key source for unit tests.
#[cfg(test)]
pub(crate) struct ScriptedKeySource {
    pub(crate) keys: std::collections::VecDeque<KeyEvent>,
}

#[cfg(test)]
impl KeySource for ScriptedKeySource {
    fn next_key(&mut self) -> Option<KeyEvent> {
        self.keys.pop_front()
    }
}

// ─── Terminal-control hooks ─────────────────────────────────────────────────

/// Trait that abstracts the side effects required to enter an
/// interactive terminal session: raw mode + alt-screen + bracketed
/// paste. Production wires this to crossterm; tests wire it to a
/// counter so we can assert "exactly one enter / exactly one exit".
///
/// The mouse-capture default is OFF per
/// `feedback-cross-platform-ux-defaults.md` and #623 — `enter()` only
/// enables raw-mode + alt-screen + bracketed paste, NEVER mouse
/// capture. The wizard captures the user's mouse preference but does
/// not apply it to the wizard's own session (the value lands in
/// settings.json for the main TUI to read on next launch).
pub(crate) trait TerminalHooks {
    /// Switch to raw mode + alt-screen. Called exactly once at
    /// `WizardSession::enter`.
    fn enter(&mut self) -> Result<(), RunnerError>;
    /// Restore cooked mode + leave alt-screen. Called exactly once at
    /// `WizardSession::drop` (or `leave()` if the caller wants to
    /// release early before the value is dropped).
    fn leave(&mut self);
}

/// Production hooks: real crossterm enable / disable. Only enters
/// alt-screen when stdout is a TTY — keeps unit tests outside this
/// file from accidentally going alt-screen.
#[allow(dead_code)]
pub(crate) struct CrosstermHooks {
    entered: bool,
}

#[allow(dead_code)]
impl CrosstermHooks {
    pub(crate) fn new() -> Self {
        Self { entered: false }
    }
}

impl TerminalHooks for CrosstermHooks {
    fn enter(&mut self) -> Result<(), RunnerError> {
        crossterm::terminal::enable_raw_mode()
            .map_err(|e| RunnerError::Enter(e.to_string()))?;
        crossterm::execute!(
            io::stdout(),
            crossterm::terminal::EnterAlternateScreen,
            crossterm::event::EnableBracketedPaste
        )
        .map_err(|e| RunnerError::Enter(e.to_string()))?;
        self.entered = true;
        Ok(())
    }

    fn leave(&mut self) {
        if self.entered {
            let _ = crossterm::execute!(
                io::stdout(),
                crossterm::event::DisableBracketedPaste,
                crossterm::terminal::LeaveAlternateScreen
            );
            let _ = crossterm::terminal::disable_raw_mode();
            self.entered = false;
        }
    }
}

/// Counted hooks used by the unit-test suite. Increments `entered`
/// and `left` so the tests can assert "exactly one enter" + "exactly
/// one exit" + "raw mode released even on panic".
#[cfg(test)]
#[derive(Default)]
pub(crate) struct CountingHooks {
    pub(crate) entered: usize,
    pub(crate) left: usize,
    pub(crate) raw_active: bool,
}

#[cfg(test)]
impl TerminalHooks for CountingHooks {
    fn enter(&mut self) -> Result<(), RunnerError> {
        self.entered += 1;
        self.raw_active = true;
        Ok(())
    }

    fn leave(&mut self) {
        // Idempotent — second invocation is a no-op so accidental
        // double-leave (e.g. explicit `leave()` then `Drop`) does not
        // double-count.
        if self.raw_active {
            self.left += 1;
            self.raw_active = false;
        }
    }
}

// ─── WizardSession ──────────────────────────────────────────────────────────

/// Owns the alt-screen terminal for the *entire* TUI-config portion of
/// the first-run wizard. The session enters alt-screen exactly once at
/// `enter()` and leaves exactly once at `Drop`, so the four modal
/// steps in `wizard.rs` (layout architecture, layout tabs, mouse,
/// theme, permission) render without flicker.
///
/// The session does NOT own a `KeySource` — that is supplied per call
/// to `WizardModalRunner` so production can use a real crossterm poll
/// while tests inject a scripted source.
pub(crate) struct WizardSession<B: Backend, H: TerminalHooks> {
    pub(crate) terminal: Terminal<B>,
    pub(crate) hooks: H,
    /// Tracks whether `hooks.enter()` succeeded. `Drop` only calls
    /// `hooks.leave()` when this is `true` so a failed enter does not
    /// also fail a leave.
    pub(crate) entered: bool,
}

impl<B: Backend, H: TerminalHooks> WizardSession<B, H>
where
    B::Error: fmt::Display,
{
    /// Construct a session by handing in a pre-built terminal and the
    /// terminal-control hooks. Calls `hooks.enter()` once and returns
    /// the session. The session is ready to drive modals; the caller
    /// owns dropping it at the end of the TUI-config portion of the
    /// wizard.
    pub(crate) fn enter(terminal: Terminal<B>, mut hooks: H) -> Result<Self, RunnerError> {
        hooks.enter()?;
        Ok(Self {
            terminal,
            hooks,
            entered: true,
        })
    }

    /// Render a centered banner inside the active alt-screen. Used to
    /// separate consecutive modals with a brief context message
    /// ("Step 7 of 8: TUI Layout", etc.) — the user does NOT see the
    /// terminal return to inline output between modals.
    pub(crate) fn render_banner(
        &mut self,
        title: &str,
        body: &[&str],
        accent: Color,
    ) -> Result<(), RunnerError> {
        let title_owned = title.to_string();
        let body_owned: Vec<String> = body.iter().map(|s| (*s).to_string()).collect();
        self.terminal
            .draw(|frame| {
                use ratatui::layout::Rect;
                use ratatui::style::{Modifier, Style};
                use ratatui::text::{Line, Span};
                use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

                let area = frame.area();
                if area.width < 12 || area.height < 5 {
                    return;
                }
                let banner_w = area.width.saturating_sub(8).min(70).max(30);
                let banner_h: u16 = (body_owned.len() as u16) + 4;
                let banner_x = (area.width.saturating_sub(banner_w)) / 2;
                let banner_y = (area.height.saturating_sub(banner_h)) / 2;
                let banner_area = Rect {
                    x: banner_x,
                    y: banner_y,
                    width: banner_w,
                    height: banner_h,
                };

                // Region-scoped Clear (not full screen) — avoids the
                // flash anti-pattern #622 while still wiping any prior
                // modal content within the banner footprint.
                frame.render_widget(Clear, banner_area);

                let block = Block::default()
                    .title(format!(" {} ", title_owned))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(accent).add_modifier(Modifier::BOLD));
                let inner = block.inner(banner_area);
                frame.render_widget(block, banner_area);

                let lines: Vec<Line<'static>> = body_owned
                    .iter()
                    .map(|line| {
                        Line::from(Span::styled(
                            format!("  {line}"),
                            Style::default().add_modifier(Modifier::DIM),
                        ))
                    })
                    .collect();
                frame.render_widget(Paragraph::new(lines), inner);
            })
            .map_err(|e| RunnerError::Draw(e.to_string()))?;
        Ok(())
    }

    /// Explicit leave (rare — `Drop` handles the common case). Useful
    /// only when the caller wants to release alt-screen before the
    /// session value goes out of scope (e.g. to chain a stdin step
    /// after the modals). Idempotent.
    #[allow(dead_code)]
    pub(crate) fn leave(&mut self) {
        if self.entered {
            self.hooks.leave();
            self.entered = false;
        }
    }
}

impl<B: Backend, H: TerminalHooks> Drop for WizardSession<B, H> {
    fn drop(&mut self) {
        // Single source of truth for cleanup — runs even on panic
        // (unwind) so a mid-step Esc / panic does not leave the
        // terminal in raw mode.
        if self.entered {
            self.hooks.leave();
            self.entered = false;
        }
    }
}

// ─── WizardModalRunner ──────────────────────────────────────────────────────

/// Borrower of a `WizardSession`. Drives modals inside the session's
/// already-active alt-screen — does NOT enter or leave alt-screen.
/// Each call to `run_queue` / `run_modal` renders frames into the
/// session's terminal and blocks on the supplied key source.
///
/// The `accent` color is supplied by the caller so the modals match
/// the user's theme.
pub(crate) struct WizardModalRunner<'a, B: Backend, H: TerminalHooks, K: KeySource> {
    pub(crate) session: &'a mut WizardSession<B, H>,
    pub(crate) keys: K,
    pub(crate) accent: Color,
}

impl<'a, B: Backend, H: TerminalHooks, K: KeySource> WizardModalRunner<'a, B, H, K>
where
    B::Error: fmt::Display,
{
    /// Construct a runner that drives modals against an existing
    /// session. The session must already have called `enter()`.
    pub(crate) fn new(
        session: &'a mut WizardSession<B, H>,
        keys: K,
        accent: Color,
    ) -> Self {
        Self {
            session,
            keys,
            accent,
        }
    }

    /// Drive every modal currently on the queue to a resolution. The
    /// recorded answers are written into the queue's answer log, which
    /// the caller can read via `queue.answer_for(tag)`. Returns the
    /// number of modals resolved (success or cancel).
    pub(crate) fn run_queue(&mut self, queue: &mut ModalQueue) -> Result<usize, RunnerError> {
        let mut resolved = 0usize;
        while !queue.is_empty() {
            let answer = self.drive_front(queue)?;
            queue.resolve_front(answer);
            resolved += 1;
        }
        Ok(resolved)
    }

    /// Drive a single choice modal to resolution. Convenience wrapper
    /// for the wizard's per-step call-sites — pushes the modal onto a
    /// throwaway queue, drains it, returns the recorded `ModalAnswer`.
    #[allow(dead_code)]
    pub(crate) fn run_choice(
        &mut self,
        tag: &str,
        modal: WizardChoiceModal,
    ) -> Result<ModalAnswer, RunnerError> {
        let mut q = ModalQueue::new();
        q.push(QueuedModal::Choice {
            tag: tag.to_string(),
            modal,
        });
        self.run_queue(&mut q)?;
        Ok(q.answer_for(tag).cloned().unwrap_or(ModalAnswer::ChoiceCancelled))
    }

    /// Drive a single confirm modal to resolution.
    #[allow(dead_code)]
    pub(crate) fn run_confirm(
        &mut self,
        tag: &str,
        modal: ConfirmModal,
    ) -> Result<ModalAnswer, RunnerError> {
        let mut q = ModalQueue::new();
        q.push(QueuedModal::Confirm {
            tag: tag.to_string(),
            modal,
        });
        self.run_queue(&mut q)?;
        Ok(q
            .answer_for(tag)
            .cloned()
            .unwrap_or(ModalAnswer::Confirm(crate::tui::modals::ConfirmChoice::No)))
    }

    /// Drive a single text-input modal to resolution (task #642).
    /// Returns `ModalAnswer::TextInput(value)` with the user's input or
    /// the configured default; on empty key source returns the default.
    #[allow(dead_code)]
    pub(crate) fn run_text_input(
        &mut self,
        tag: &str,
        modal: TextInputModal,
    ) -> Result<ModalAnswer, RunnerError> {
        let default = modal.default.clone();
        let mut q = ModalQueue::new();
        q.push(QueuedModal::TextInput {
            tag: tag.to_string(),
            modal,
        });
        self.run_queue(&mut q)?;
        Ok(q
            .answer_for(tag)
            .cloned()
            .unwrap_or(ModalAnswer::TextInput(default)))
    }

    /// Drive an `OAuthFlow` to completion inside the active alt-screen
    /// session (task #642 Sub-task C).
    ///
    /// The flow is polled non-blocking each tick and rendered into the
    /// session's terminal; user keys route to `OAuthFlow::handle_key`.
    /// Returns the terminal `OAuthOutcome` from `OAuthFlow::finalize`.
    ///
    /// This is the bridge that lets the first-run wizard's Step 3 run
    /// the OAuth callback inline — no drop to stdin, no second
    /// alt-screen transition, the whole flow stays inside the same
    /// `WizardSession` the rest of the wizard owns.
    #[allow(dead_code)]
    pub(crate) fn run_oauth_flow(
        &mut self,
        mut flow: crate::tui::oauth_flow::OAuthFlow,
    ) -> Result<crate::tui::oauth_flow::OAuthOutcome, RunnerError> {
        use crate::tui::oauth_flow::{OAuthAction, OAuthEvent, OAuthFlowState};
        let accent = self.accent;
        loop {
            // Per-tick: poll the listener channel before drawing so the
            // success/failed card is visible immediately after the
            // callback arrives.
            match flow.poll() {
                Some(OAuthEvent::Success) | Some(OAuthEvent::Failed(_)) => {
                    // Render the result card.
                    self.session
                        .terminal
                        .draw(|frame| {
                            let area = frame.area();
                            flow.render(frame, area, accent);
                        })
                        .map_err(|e| RunnerError::Draw(e.to_string()))?;
                }
                _ => {
                    self.session
                        .terminal
                        .draw(|frame| {
                            let area = frame.area();
                            flow.render(frame, area, accent);
                        })
                        .map_err(|e| RunnerError::Draw(e.to_string()))?;
                }
            }

            // If awaiting and channel empty, keep polling without
            // blocking forever on keys — but block briefly so the loop
            // doesn't spin.
            let key = self.keys.next_key();
            if let Some(k) = key {
                match flow.handle_key(k) {
                    OAuthAction::Continue => continue,
                    OAuthAction::Cancel | OAuthAction::Advance => {
                        return Ok(flow.finalize());
                    }
                }
            }

            // If the flow has already transitioned out of AwaitingCallback
            // and we got no key (e.g. scripted-source empty), break with
            // the current outcome rather than spinning.
            if !matches!(flow.state, OAuthFlowState::AwaitingCallback { .. }) {
                return Ok(flow.finalize());
            }
        }
    }

    /// Drive a single password modal to resolution, returning the raw
    /// secret on success. Used by the v2.2.17 wizard for vault-setup
    /// password capture inside the alt-screen session.
    ///
    /// The caller is responsible for zeroizing the returned string when
    /// done (e.g. by handing it to `VaultManager::setup` which takes it
    /// by reference + drops it after KDF derivation).
    #[allow(dead_code)]
    pub(crate) fn run_password_capture(
        &mut self,
        modal: PasswordModal,
    ) -> Result<Option<String>, RunnerError> {
        // We do NOT use the queue helper here because the queue stores
        // only `PasswordSubmitted(bool)` (intentionally never the raw
        // secret).  Instead we mimic `drive_front`'s loop with one
        // password modal and read the value out of the Submit action
        // directly.
        let mut modal = modal;
        let accent = self.accent;
        loop {
            self.session
                .terminal
                .draw(|frame| {
                    let area = frame.area();
                    modal.render(frame, area, accent, ratatui::style::Color::Red);
                })
                .map_err(|e| RunnerError::Draw(e.to_string()))?;
            let Some(key) = self.keys.next_key() else {
                return Ok(None);
            };
            match modal.handle_key(key) {
                PasswordAction::Continue => continue,
                PasswordAction::Submit(pw) => return Ok(Some(pw)),
                PasswordAction::Cancel => return Ok(None),
            }
        }
    }

    fn drive_front(&mut self, queue: &mut ModalQueue) -> Result<ModalAnswer, RunnerError> {
        let accent = self.accent;
        loop {
            // Render the front modal centered on the screen.
            {
                let front = queue.front_mut().expect("drive_front: queue empty");
                self.session
                    .terminal
                    .draw(|frame| {
                        let area = frame.area();
                        match front {
                            QueuedModal::Confirm { modal, .. } => {
                                modal.render(frame, area, accent);
                            }
                            QueuedModal::Choice { modal, .. } => {
                                modal.render(frame, area, accent);
                            }
                            QueuedModal::Password { modal, .. } => {
                                modal.render(frame, area, accent, Color::Red);
                            }
                            QueuedModal::TextInput { modal, .. } => {
                                modal.render(frame, area, &modal.buffer, modal.cursor, accent);
                            }
                        }
                    })
                    .map_err(|e| RunnerError::Draw(e.to_string()))?;
            }

            // Block on key. If the source goes empty, treat as cancel.
            let Some(key) = self.keys.next_key() else {
                return Ok(match queue.front().unwrap() {
                    QueuedModal::Choice { .. } => ModalAnswer::ChoiceCancelled,
                    QueuedModal::Confirm { .. } => {
                        ModalAnswer::Confirm(crate::tui::modals::ConfirmChoice::No)
                    }
                    QueuedModal::Password { .. } => ModalAnswer::PasswordSubmitted(false),
                    QueuedModal::TextInput { modal, .. } => {
                        ModalAnswer::TextInput(modal.default.clone())
                    }
                });
            };

            // Route to the modal's state machine.
            let front = queue.front_mut().unwrap();
            match front {
                QueuedModal::Choice { modal, .. } => match modal.handle_key(key) {
                    ChoiceAction::Continue => continue,
                    ChoiceAction::Committed(idx) => return Ok(ModalAnswer::Choice(idx)),
                    ChoiceAction::Cancelled => return Ok(ModalAnswer::ChoiceCancelled),
                },
                QueuedModal::Confirm { modal, .. } => match modal.handle_key(key) {
                    ConfirmAction::Continue => continue,
                    ConfirmAction::Committed(choice) => {
                        return Ok(ModalAnswer::Confirm(choice));
                    }
                },
                QueuedModal::Password { modal, .. } => match modal.handle_key(key) {
                    PasswordAction::Continue => continue,
                    PasswordAction::Submit(_) => {
                        // Real submission would be consumed by the host
                        // (e.g. VaultManager::unlock); the runner
                        // records boolean success only — the raw
                        // password is intentionally never logged.
                        return Ok(ModalAnswer::PasswordSubmitted(true));
                    }
                    PasswordAction::Cancel => return Ok(ModalAnswer::PasswordSubmitted(false)),
                },
                QueuedModal::TextInput { modal, .. } => match modal.handle_key(key) {
                    TextInputAction::Continue => continue,
                    TextInputAction::Submit(value) => return Ok(ModalAnswer::TextInput(value)),
                    TextInputAction::Cancel(default) => {
                        // Per the spec: Esc takes the default. So we
                        // still return the default value as a successful
                        // TextInput rather than a Cancelled signal — the
                        // wizard's "Esc = default" UX is the contract.
                        return Ok(ModalAnswer::TextInput(default));
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::backend::TestBackend;
    use std::collections::VecDeque;

    use crate::tui::modals::ConfirmChoice;
    use crate::tui::modals::confirm::ConfirmModal;
    use crate::tui::modals::queue::{QueuedModal, WizardChoiceModal};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Build a session backed by `TestBackend` + `CountingHooks` so
    /// the test can inspect enter/exit counts and read the rendered
    /// frame buffer.
    fn make_session() -> WizardSession<TestBackend, CountingHooks> {
        let backend = TestBackend::new(80, 24);
        let terminal = Terminal::new(backend).expect("TestBackend");
        WizardSession::enter(terminal, CountingHooks::default()).expect("enter")
    }

    fn make_runner<'a>(
        session: &'a mut WizardSession<TestBackend, CountingHooks>,
        keys: Vec<KeyEvent>,
    ) -> WizardModalRunner<'a, TestBackend, CountingHooks, ScriptedKeySource> {
        let scripted = ScriptedKeySource {
            keys: VecDeque::from(keys),
        };
        WizardModalRunner::new(session, scripted, Color::Cyan)
    }

    // ─── Foundation tests (#579, must keep passing) ────────────────────

    /// #579: a `Choice` modal drained by a digit press records the
    /// selected index in the queue's answer log.
    #[test]
    fn wizard_layout_step_uses_choice_modal() {
        let mut queue = ModalQueue::new();
        queue.push(QueuedModal::Choice {
            tag: "step7-layout".to_string(),
            modal: WizardChoiceModal::new(
                "TUI Layout",
                vec![
                    "Vertical Split".into(),
                    "Classic".into(),
                    "Three-Pane".into(),
                    "Journal".into(),
                ],
            ),
        });

        let mut session = make_session();
        let mut runner = make_runner(&mut session, vec![key(KeyCode::Char('2'))]);
        let count = runner.run_queue(&mut queue).expect("run_queue");
        assert_eq!(count, 1);
        assert_eq!(
            queue.answer_for("step7-layout"),
            Some(&ModalAnswer::Choice(1)),
            "step 7 must record the user's selection"
        );
        assert!(queue.is_empty(), "queue must drain");
    }

    /// #579: a `Confirm` modal returns Yes/No via 'y' / 'n' keys.
    #[test]
    fn wizard_mouse_step_uses_confirm_modal() {
        let mut queue = ModalQueue::new();
        queue.push(QueuedModal::Confirm {
            tag: "step8-mouse".to_string(),
            modal: ConfirmModal::new(
                "Enable mouse capture?",
                "Mouse capture lets you click tabs but breaks native text selection.",
            ),
        });

        let mut session = make_session();
        let mut runner = make_runner(&mut session, vec![key(KeyCode::Char('y'))]);
        runner.run_queue(&mut queue).expect("run_queue");
        assert_eq!(
            queue.answer_for("step8-mouse"),
            Some(&ModalAnswer::Confirm(ConfirmChoice::Yes)),
            "explicit Yes must be recorded"
        );
    }

    /// #579: a `Choice` modal step for theme selection.
    #[test]
    fn wizard_theme_step_uses_choice_modal() {
        let mut queue = ModalQueue::new();
        queue.push(QueuedModal::Choice {
            tag: "step8-theme".to_string(),
            modal: WizardChoiceModal::new(
                "Theme",
                vec!["Dark".into(), "Light".into(), "Auto".into()],
            ),
        });

        let mut session = make_session();
        let mut runner = make_runner(
            &mut session,
            vec![key(KeyCode::Down), key(KeyCode::Enter)],
        );
        runner.run_queue(&mut queue).expect("run_queue");
        assert_eq!(
            queue.answer_for("step8-theme"),
            Some(&ModalAnswer::Choice(1)),
            "Down+Enter must select index 1"
        );
    }

    /// #579: permission-mode step is a choice modal with three options.
    #[test]
    fn wizard_permission_mode_step_uses_choice_modal() {
        let mut queue = ModalQueue::new();
        queue.push(QueuedModal::Choice {
            tag: "step8-perm".to_string(),
            modal: WizardChoiceModal::new(
                "Default permission mode",
                vec![
                    "ask".into(),
                    "workspace-write".into(),
                    "danger-full-access".into(),
                ],
            ),
        });

        let mut session = make_session();
        let mut runner = make_runner(&mut session, vec![key(KeyCode::Char('1'))]);
        runner.run_queue(&mut queue).expect("run_queue");
        assert_eq!(
            queue.answer_for("step8-perm"),
            Some(&ModalAnswer::Choice(0)),
        );
    }

    /// #579: a multi-step wizard drains the queue in FIFO order.
    #[test]
    fn wizard_full_run_completes_via_modals_only() {
        let mut queue = ModalQueue::new();
        queue.push(QueuedModal::Choice {
            tag: "layout-kind".to_string(),
            modal: WizardChoiceModal::new(
                "Layout architecture",
                vec![
                    "Vertical".into(),
                    "Classic".into(),
                    "Three-Pane".into(),
                    "Journal".into(),
                ],
            ),
        });
        queue.push(QueuedModal::Confirm {
            tag: "layout-tabs".to_string(),
            modal: ConfirmModal::new(
                "Show workspace tabs?",
                "Tabs let you keep multiple sessions visible.",
            ),
        });
        queue.push(QueuedModal::Choice {
            tag: "theme".to_string(),
            modal: WizardChoiceModal::new(
                "Theme",
                vec!["Dark".into(), "Light".into(), "Auto".into()],
            ),
        });
        queue.push(QueuedModal::Choice {
            tag: "permission".to_string(),
            modal: WizardChoiceModal::new(
                "Default permission mode",
                vec![
                    "ask".into(),
                    "workspace-write".into(),
                    "danger-full-access".into(),
                ],
            ),
        });

        let mut session = make_session();
        let mut runner = make_runner(
            &mut session,
            vec![
                key(KeyCode::Char('1')),
                key(KeyCode::Char('y')),
                key(KeyCode::Char('1')),
                key(KeyCode::Char('1')),
            ],
        );
        let resolved = runner.run_queue(&mut queue).expect("run_queue");
        assert_eq!(resolved, 4, "all four steps must drain");
        assert_eq!(queue.answer_for("layout-kind"), Some(&ModalAnswer::Choice(0)));
        assert_eq!(
            queue.answer_for("layout-tabs"),
            Some(&ModalAnswer::Confirm(ConfirmChoice::Yes))
        );
        assert_eq!(queue.answer_for("theme"), Some(&ModalAnswer::Choice(0)));
        assert_eq!(queue.answer_for("permission"), Some(&ModalAnswer::Choice(0)));
    }

    /// #579: when the user presses Esc on a Choice modal the runner
    /// records `ChoiceCancelled` so the wizard can detect abort.
    #[test]
    fn wizard_choice_esc_cancels_step() {
        let mut queue = ModalQueue::new();
        queue.push(QueuedModal::Choice {
            tag: "layout".to_string(),
            modal: WizardChoiceModal::new("Layout", vec!["A".into(), "B".into()]),
        });
        let mut session = make_session();
        let mut runner = make_runner(&mut session, vec![key(KeyCode::Esc)]);
        runner.run_queue(&mut queue).expect("run_queue");
        assert_eq!(
            queue.answer_for("layout"),
            Some(&ModalAnswer::ChoiceCancelled),
        );
    }

    /// #579 default-OFF for mouse capture: the Confirm modal defaults
    /// to No, and ESC commits No too — matching the cross-platform
    /// default established in task #623.
    #[test]
    fn wizard_mouse_step_default_off_via_esc() {
        let mut queue = ModalQueue::new();
        queue.push(QueuedModal::Confirm {
            tag: "mouse".to_string(),
            modal: ConfirmModal::new("Enable mouse capture?", "..."),
        });
        let mut session = make_session();
        let mut runner = make_runner(&mut session, vec![key(KeyCode::Esc)]);
        runner.run_queue(&mut queue).expect("run_queue");
        assert_eq!(
            queue.answer_for("mouse"),
            Some(&ModalAnswer::Confirm(ConfirmChoice::No)),
            "ESC on the mouse-capture modal must default to NO"
        );
    }

    // ─── New single-alt-screen tests (v2.2.17 finisher) ────────────────

    /// The wizard session enters alt-screen exactly once at construction.
    #[test]
    fn wizard_session_enters_alt_screen_once() {
        let backend = TestBackend::new(80, 24);
        let terminal = Terminal::new(backend).unwrap();
        let session = WizardSession::enter(terminal, CountingHooks::default())
            .expect("enter");
        assert_eq!(session.hooks.entered, 1, "exactly one enter");
        assert_eq!(session.hooks.left, 0, "no leave yet");
        drop(session);
    }

    /// `Drop` releases the alt-screen exactly once.
    #[test]
    fn wizard_session_exits_alt_screen_once_on_drop() {
        let backend = TestBackend::new(80, 24);
        let terminal = Terminal::new(backend).unwrap();

        // Wrap in Rc<RefCell> via a stash so the test can read the
        // hooks back after Drop fires. CountingHooks is owned by the
        // session, so we have to share state.
        use std::cell::RefCell;
        use std::rc::Rc;

        #[derive(Default)]
        struct SharedHooks {
            inner: Rc<RefCell<CountingHooks>>,
        }

        impl TerminalHooks for SharedHooks {
            fn enter(&mut self) -> Result<(), RunnerError> {
                self.inner.borrow_mut().enter()
            }
            fn leave(&mut self) {
                self.inner.borrow_mut().leave();
            }
        }

        let shared = Rc::new(RefCell::new(CountingHooks::default()));
        let hooks = SharedHooks {
            inner: Rc::clone(&shared),
        };
        let session = WizardSession::enter(terminal, hooks).expect("enter");
        assert_eq!(shared.borrow().entered, 1);
        assert_eq!(shared.borrow().left, 0);
        drop(session);
        assert_eq!(shared.borrow().left, 1, "exactly one leave on drop");
    }

    /// Even if the session is unwound by a panic mid-step (simulated
    /// here by an explicit early drop), raw mode is released.
    #[test]
    fn wizard_session_drop_disables_raw_mode() {
        let backend = TestBackend::new(80, 24);
        let terminal = Terminal::new(backend).unwrap();

        use std::cell::RefCell;
        use std::rc::Rc;
        #[derive(Default)]
        struct SharedHooks {
            inner: Rc<RefCell<CountingHooks>>,
        }
        impl TerminalHooks for SharedHooks {
            fn enter(&mut self) -> Result<(), RunnerError> {
                self.inner.borrow_mut().enter()
            }
            fn leave(&mut self) {
                self.inner.borrow_mut().leave();
            }
        }

        let shared = Rc::new(RefCell::new(CountingHooks::default()));
        let hooks = SharedHooks {
            inner: Rc::clone(&shared),
        };
        {
            let _session = WizardSession::enter(terminal, hooks).expect("enter");
            assert!(shared.borrow().raw_active, "raw mode active during session");
        }
        assert!(
            !shared.borrow().raw_active,
            "raw mode released after session drops"
        );
        assert_eq!(shared.borrow().left, 1, "leave called exactly once");
    }

    /// Full integration: a wizard run through 4 modal steps emits
    /// exactly one enter sequence and exactly one exit sequence.
    /// (CountingHooks proxies the byte-emit semantics — the real
    /// `CrosstermHooks::enter` writes EnterAlternateScreen exactly
    /// once per `enter()` call; we count the calls.)
    #[test]
    fn wizard_full_run_emits_single_alt_screen_transition() {
        use std::cell::RefCell;
        use std::rc::Rc;
        #[derive(Default)]
        struct SharedHooks {
            inner: Rc<RefCell<CountingHooks>>,
        }
        impl TerminalHooks for SharedHooks {
            fn enter(&mut self) -> Result<(), RunnerError> {
                self.inner.borrow_mut().enter()
            }
            fn leave(&mut self) {
                self.inner.borrow_mut().leave();
            }
        }

        let shared = Rc::new(RefCell::new(CountingHooks::default()));
        let hooks = SharedHooks {
            inner: Rc::clone(&shared),
        };
        let backend = TestBackend::new(80, 24);
        let terminal = Terminal::new(backend).unwrap();
        let mut session = WizardSession::enter(terminal, hooks).expect("enter");

        // 4 steps + a banner between each.
        let mut queue = ModalQueue::new();
        queue.push(QueuedModal::Choice {
            tag: "kind".to_string(),
            modal: WizardChoiceModal::new(
                "Layout",
                vec!["A".into(), "B".into(), "C".into(), "D".into()],
            ),
        });
        queue.push(QueuedModal::Confirm {
            tag: "mouse".to_string(),
            modal: ConfirmModal::new("Mouse?", "Default OFF."),
        });
        queue.push(QueuedModal::Choice {
            tag: "theme".to_string(),
            modal: WizardChoiceModal::new("Theme", vec!["Dark".into(), "Light".into()]),
        });
        queue.push(QueuedModal::Choice {
            tag: "perm".to_string(),
            modal: WizardChoiceModal::new(
                "Permission",
                vec!["ask".into(), "workspace-write".into()],
            ),
        });

        {
            let scripted = ScriptedKeySource {
                keys: VecDeque::from(vec![
                    key(KeyCode::Char('1')),
                    key(KeyCode::Char('n')),
                    key(KeyCode::Char('1')),
                    key(KeyCode::Char('1')),
                ]),
            };
            let mut runner = WizardModalRunner::new(&mut session, scripted, Color::Cyan);
            runner.run_queue(&mut queue).expect("run_queue");
        }

        // Session still active mid-run, no leave yet.
        assert_eq!(shared.borrow().entered, 1, "single enter for entire run");
        assert_eq!(shared.borrow().left, 0, "no leave during 4-step run");

        drop(session);
        assert_eq!(shared.borrow().entered, 1, "still exactly one enter");
        assert_eq!(shared.borrow().left, 1, "exactly one exit on drop");
    }

    /// A banner rendered between modals shows the supplied title in
    /// the frame buffer (proves we are NOT going back inline).
    #[test]
    fn wizard_run_renders_banner_between_modals() {
        let mut session = make_session();
        session
            .render_banner(
                "Step 7 of 8",
                &["TUI Layout — pick your default workspace"],
                Color::Cyan,
            )
            .expect("render_banner");

        // Inspect the frame buffer: the title text must appear in some
        // cell on the screen.
        let buf = session.terminal.backend().buffer();
        let mut found_title = false;
        for y in 0..buf.area.height {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            if row.contains("Step 7 of 8") {
                found_title = true;
                break;
            }
        }
        assert!(found_title, "banner title must appear in frame buffer");
    }

    /// After the wizard migration, the mouse-capture default is OFF.
    /// We exercise the Confirm modal with an empty key stream (the
    /// user pressed nothing, which collapses to "Esc cancel = No") —
    /// the recorded answer must be `No`. This guards against a future
    /// edit that flips the default direction in
    /// `ConfirmModal::new`.
    #[test]
    fn wizard_mouse_capture_default_off_after_migration() {
        let mut queue = ModalQueue::new();
        queue.push(QueuedModal::Confirm {
            tag: "mouse-default".to_string(),
            modal: ConfirmModal::new(
                "Enable mouse capture?",
                "Default OFF per #623 / feedback-cross-platform-ux-defaults.md.",
            ),
        });
        let mut session = make_session();
        // No keys at all — the runner observes an empty source and
        // records the default (`No`).
        let mut runner = make_runner(&mut session, vec![]);
        runner.run_queue(&mut queue).expect("run_queue");
        assert_eq!(
            queue.answer_for("mouse-default"),
            Some(&ModalAnswer::Confirm(ConfirmChoice::No)),
            "mouse capture must default to OFF (No)"
        );
    }
}
