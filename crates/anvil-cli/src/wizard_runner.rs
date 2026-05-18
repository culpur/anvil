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
//! The bridge is `WizardModalRunner`: it owns its own
//! `Terminal<CrosstermBackend<Stdout>>` (or a `TestBackend` for unit
//! tests), drains a supplied `ModalQueue` one entry at a time, and
//! returns the recorded `ModalAnswer`s.
//!
//! ## 8-axis capability contract
//!
//! 1. Definition       — `WizardModalRunner` + `WizardRunnerKey` here.
//! 2. Registration     — exposed via `mod wizard_runner` in `main.rs`.
//! 3. Completion       — N/A (a runtime helper, not a slash command).
//! 4. Handler          — `run_queue` drives `handle_key` per modal.
//! 5. Dispatch         — wizard.rs callers will enqueue + invoke.
//! 6. Rendering        — delegates to each modal's `render` method.
//! 7. Gate             — single live modal at a time (top of queue).
//! 8. OTel + tests     — unit tests at the bottom of this file cover
//!                       FIFO draining + answer capture + Esc cancel.
//!
//! ## Exceptions (call-site responsibility, see `feedback-anvil-capability-contract.md`)
//!
//! Provider OAuth login (`run_anthropic_login`) and vault password
//! prompts (`rpassword::prompt_password`) intentionally bypass this
//! runner — they need stdin echo control that ratatui's alt-screen
//! semantics cannot replicate. Those two sites stay on the existing
//! stdin path; see wizard.rs steps 1 + 3 + "[2] manual key" branches.

use std::fmt;
use std::time::Duration;

use crossterm::event::{Event, KeyEvent};

use ratatui::backend::Backend;
use ratatui::style::Color;
use ratatui::Terminal;

/// Errors emitted by `WizardModalRunner::run_queue`.
#[derive(Debug)]
pub(crate) enum RunnerError {
    /// The backing terminal failed to draw a frame.
    Draw(String),
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Draw(msg) => write!(f, "wizard runner draw error: {msg}"),
        }
    }
}

impl std::error::Error for RunnerError {}

use crate::tui::modals::confirm::ConfirmAction;
use crate::tui::modals::password::PasswordAction;
use crate::tui::modals::queue::{ChoiceAction, ModalAnswer, ModalQueue, QueuedModal};

/// Source of key events for the runner. Production uses
/// `CrosstermKeySource` which polls real terminal input; tests use
/// `ScriptedKeySource` to inject a deterministic sequence.
pub(crate) trait KeySource {
    /// Block until the next `KeyEvent` (or return `None` on cancel).
    fn next_key(&mut self) -> Option<KeyEvent>;
}

/// Production key source: polls crossterm's event stream.
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

/// Backend-parameterized runner. The `accent` color is supplied by the
/// caller so the runner matches the user's theme.
pub(crate) struct WizardModalRunner<B: Backend, K: KeySource> {
    pub(crate) terminal: Terminal<B>,
    pub(crate) keys: K,
    pub(crate) accent: Color,
}

impl<B: Backend, K: KeySource> WizardModalRunner<B, K>
where
    B::Error: fmt::Display,
{
    /// Drive every modal currently on the queue to a resolution. The
    /// recorded answers are written into the queue's answer log, which
    /// the caller can read via `queue.answer_for(tag)`. Returns the
    /// number of modals resolved (success or cancel).
    ///
    /// Each frame:
    ///   1. Borrow the front modal and render it via its `render` method.
    ///   2. Block on the key source for the next event.
    ///   3. Hand the key to the modal's state machine.
    ///   4. On a terminal action (Committed / Cancelled / etc.), pop
    ///      the modal and record the answer.
    pub(crate) fn run_queue(&mut self, queue: &mut ModalQueue) -> Result<usize, RunnerError> {
        let mut resolved = 0usize;
        while !queue.is_empty() {
            // Each iteration: draw the current front modal, then wait for
            // a key, then update the modal. We loop until the modal
            // resolves (Committed / Cancelled / submitted).
            let answer = self.drive_front(queue)?;
            queue.resolve_front(answer);
            resolved += 1;
        }
        Ok(resolved)
    }

    fn drive_front(&mut self, queue: &mut ModalQueue) -> Result<ModalAnswer, RunnerError> {
        let accent = self.accent;
        loop {
            // Render the front modal centered on the screen.
            {
                let front = queue.front_mut().expect("drive_front: queue empty");
                self.terminal
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
                });
            };

            // Route to the modal's state machine.
            let front = queue.front_mut().unwrap();
            match front {
                QueuedModal::Choice { modal, .. } => {
                    match modal.handle_key(key) {
                        ChoiceAction::Continue => continue,
                        ChoiceAction::Committed(idx) => return Ok(ModalAnswer::Choice(idx)),
                        ChoiceAction::Cancelled => return Ok(ModalAnswer::ChoiceCancelled),
                    }
                }
                QueuedModal::Confirm { modal, .. } => {
                    match modal.handle_key(key) {
                        ConfirmAction::Continue => continue,
                        ConfirmAction::Committed(choice) => {
                            return Ok(ModalAnswer::Confirm(choice));
                        }
                    }
                }
                QueuedModal::Password { modal, .. } => {
                    match modal.handle_key(key) {
                        PasswordAction::Continue => continue,
                        PasswordAction::Submit(_) => {
                            // Real submission would be consumed by the host
                            // (e.g. VaultManager::unlock); the runner
                            // records boolean success only — the raw
                            // password is intentionally never logged.
                            return Ok(ModalAnswer::PasswordSubmitted(true));
                        }
                        PasswordAction::Cancel => return Ok(ModalAnswer::PasswordSubmitted(false)),
                    }
                }
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

    use crate::tui::modals::confirm::ConfirmModal;
    use crate::tui::modals::queue::{QueuedModal, WizardChoiceModal};
    use crate::tui::modals::ConfirmChoice;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn make_runner(
        keys: Vec<KeyEvent>,
    ) -> WizardModalRunner<TestBackend, ScriptedKeySource> {
        let backend = TestBackend::new(80, 24);
        let terminal = Terminal::new(backend).expect("TestBackend");
        let scripted = ScriptedKeySource {
            keys: VecDeque::from(keys),
        };
        WizardModalRunner {
            terminal,
            keys: scripted,
            accent: Color::Cyan,
        }
    }

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

        // User presses '2' to pick "Classic".
        let mut runner = make_runner(vec![key(KeyCode::Char('2'))]);
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

        // User explicitly opts in with 'y'.
        let mut runner = make_runner(vec![key(KeyCode::Char('y'))]);
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

        // Down + Enter selects "Light" (index 1).
        let mut runner = make_runner(vec![
            key(KeyCode::Down),
            key(KeyCode::Enter),
        ]);
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

        // Press '1' to keep the conservative default.
        let mut runner = make_runner(vec![key(KeyCode::Char('1'))]);
        runner.run_queue(&mut queue).expect("run_queue");
        assert_eq!(
            queue.answer_for("step8-perm"),
            Some(&ModalAnswer::Choice(0)),
        );
    }

    /// #579: a multi-step wizard drains the queue in FIFO order and
    /// every answer is keyed by its tag (matches `wizard_full_run_completes_via_modals_only`
    /// in the task spec).
    #[test]
    fn wizard_full_run_completes_via_modals_only() {
        let mut queue = ModalQueue::new();
        // 4 steps in order: layout-kind, layout-tabs, theme, permission.
        queue.push(QueuedModal::Choice {
            tag: "layout-kind".to_string(),
            modal: WizardChoiceModal::new(
                "Layout architecture",
                vec!["Vertical".into(), "Classic".into(), "Three-Pane".into(), "Journal".into()],
            ),
        });
        queue.push(QueuedModal::Confirm {
            tag: "layout-tabs".to_string(),
            modal: ConfirmModal::new("Show workspace tabs?", "Tabs let you keep multiple sessions visible."),
        });
        queue.push(QueuedModal::Choice {
            tag: "theme".to_string(),
            modal: WizardChoiceModal::new("Theme", vec!["Dark".into(), "Light".into(), "Auto".into()]),
        });
        queue.push(QueuedModal::Choice {
            tag: "permission".to_string(),
            modal: WizardChoiceModal::new(
                "Default permission mode",
                vec!["ask".into(), "workspace-write".into(), "danger-full-access".into()],
            ),
        });

        // Scripted keystrokes mirror a realistic walkthrough:
        //   layout-kind  → press '1' (Vertical, default)
        //   layout-tabs  → press 'y' (tabs on, default)
        //   theme        → press '1' (Dark, default)
        //   permission   → press '1' (ask, default)
        let mut runner = make_runner(vec![
            key(KeyCode::Char('1')),
            key(KeyCode::Char('y')),
            key(KeyCode::Char('1')),
            key(KeyCode::Char('1')),
        ]);
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
        let mut runner = make_runner(vec![key(KeyCode::Esc)]);
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
        let mut runner = make_runner(vec![key(KeyCode::Esc)]);
        runner.run_queue(&mut queue).expect("run_queue");
        assert_eq!(
            queue.answer_for("mouse"),
            Some(&ModalAnswer::Confirm(ConfirmChoice::No)),
            "ESC on the mouse-capture modal must default to NO"
        );
    }
}
