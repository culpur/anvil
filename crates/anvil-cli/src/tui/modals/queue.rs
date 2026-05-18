//! Modal queue — sequenced overlay scheduling (task #579, foundation).
//!
//! The first-run wizard (`wizard.rs`) is plain-CLI prompts: stdin
//! `read_line` blocks the main thread. Task #579 wants those prompts
//! migrated to in-TUI modal overlays — keep the wizard's branching
//! logic, swap the I/O.
//!
//! This module is the **foundation**: a FIFO queue of `QueuedModal`
//! entries the host drains one at a time. Each `QueuedModal` carries a
//! tag identifying the wizard step (or any caller) and an answer slot
//! the host fills when the user commits. Callers register a chain of
//! modals upfront; the input handler advances the queue on each
//! `Committed` action.
//!
//! ## Why a queue
//!
//! The wizard has eight sequential steps. Without a queue, each step
//! would need a custom modal-open path; with a queue, the wizard
//! enqueues `[Step1, Step2, ..., Step8]` once and the input handler's
//! existing modal-resolution path pops the next entry on each
//! resolution. The queue is therefore the missing piece between the
//! single-shot ConfirmModal/PasswordModal (task #627) and a fully
//! modal-driven wizard.
//!
//! ## 8-axis capability contract
//!
//! 1. Definition       — `ModalQueue` + `QueuedModal` + `ModalAnswer`.
//! 2. Registration     — re-exported from `tui::modals` for callers.
//! 3. Completion       — N/A (queue is not a slash command).
//! 4. Handler          — `pop_next` / `record_answer` on each step.
//! 5. Dispatch         — wizard callers push, input handler drains.
//! 6. Rendering        — each `QueuedModal` carries its own renderable.
//! 7. Gate             — only one modal active at a time (top of queue).
//! 8. OTel + tests     — unit tests below cover FIFO order + answer
//!                       capture; wizard-side integration deferred per
//!                       the staged migration plan.
//!
//! ## Status
//!
//! v2.2.17: foundation only. The wizard still runs pre-TUI for its
//! provider-login and vault steps (those require stdin echo control
//! semantics ratatui's alt-screen does not provide). The simple
//! "choice" steps (layout, mouse capture, default model) are the
//! candidate first migrations and use the `WizardChoiceModal` defined
//! here.

use std::collections::VecDeque;

use super::confirm::{ConfirmModal, ConfirmChoice};
use super::password::PasswordModal;

/// One entry on the modal queue. The host pops the front, renders /
/// drives it, and on resolution writes the answer back into the
/// queue's answer log before popping the next.
#[derive(Debug)]
pub(crate) enum QueuedModal {
    /// A yes/no confirmation step. Body text is fully owned.
    Confirm {
        tag: String,
        modal: ConfirmModal,
    },
    /// A single-line password / secret capture.
    Password {
        tag: String,
        modal: PasswordModal,
    },
    /// A multi-option choice (e.g. "1 Vertical Split / 2 Classic / 3
    /// Three-Pane / 4 Journal" from wizard step 7). Renders a numbered
    /// list overlay; commit on `Enter` returns the highlighted index.
    Choice {
        tag: String,
        modal: WizardChoiceModal,
    },
}

impl QueuedModal {
    /// The tag identifies which wizard step (or which caller) produced
    /// this modal. The host uses it to look up the recorded answer
    /// after the queue drains.
    pub(crate) fn tag(&self) -> &str {
        match self {
            Self::Confirm { tag, .. } => tag,
            Self::Password { tag, .. } => tag,
            Self::Choice { tag, .. } => tag,
        }
    }
}

/// The value the user committed for a queued modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModalAnswer {
    Confirm(ConfirmChoice),
    /// Password modals do NOT store the raw secret in the answer log.
    /// The host consumes the password directly on resolution (e.g.
    /// `VaultManager::unlock`) before recording a boolean success.
    PasswordSubmitted(bool),
    /// Selected index in the choice list (0-based). `Esc` records
    /// `ChoiceCancelled` so callers can detect a wizard abort.
    Choice(usize),
    ChoiceCancelled,
}

/// FIFO queue of pending modals plus a parallel log of recorded answers.
///
/// Owned by `AnvilTui` so the input handler can drain it. Wizard-style
/// callers push their full step chain upfront, then read the answers
/// out of `answers_by_tag` after the queue is empty.
#[derive(Debug, Default)]
pub(crate) struct ModalQueue {
    pending: VecDeque<QueuedModal>,
    /// `(tag, answer)` pairs. Last write wins per tag.
    answers: Vec<(String, ModalAnswer)>,
}

impl ModalQueue {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Push a modal onto the back of the queue.
    pub(crate) fn push(&mut self, modal: QueuedModal) {
        self.pending.push_back(modal);
    }

    /// Borrow the modal at the front of the queue without removing it.
    /// Returns `None` when the queue is empty.
    pub(crate) fn front(&self) -> Option<&QueuedModal> {
        self.pending.front()
    }

    /// Borrow the front modal mutably (so the input handler can call
    /// `handle_key` on the wrapped ConfirmModal / PasswordModal / Choice).
    pub(crate) fn front_mut(&mut self) -> Option<&mut QueuedModal> {
        self.pending.front_mut()
    }

    /// Pop the front modal and record the answer keyed by its tag.
    pub(crate) fn resolve_front(&mut self, answer: ModalAnswer) -> Option<QueuedModal> {
        let popped = self.pending.pop_front()?;
        self.answers.push((popped.tag().to_string(), answer));
        Some(popped)
    }

    /// Has the queue drained?
    pub(crate) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Look up the recorded answer for a given tag (last write wins).
    pub(crate) fn answer_for(&self, tag: &str) -> Option<&ModalAnswer> {
        self.answers
            .iter()
            .rev()
            .find(|(t, _)| t == tag)
            .map(|(_, a)| a)
    }

    /// Drain all recorded answers as `(tag, answer)` pairs in insertion
    /// order. Useful for the wizard's post-resolution branch logic.
    #[allow(dead_code)]
    pub(crate) fn answers(&self) -> &[(String, ModalAnswer)] {
        &self.answers
    }
}

/// A numbered-list choice modal — the wizard's "Step 7: TUI Layout"
/// pattern (`[1] Vertical Split [2] Classic [3] Three-Pane [4]
/// Journal`). Up/Down navigate, Enter commits, Esc cancels.
///
/// The rendered overlay is intentionally minimal; the wizard adapter
/// supplies the title + option labels and reads the selected index
/// back from `ModalAnswer::Choice(idx)`.
#[derive(Debug, Clone)]
pub(crate) struct WizardChoiceModal {
    pub(crate) title: String,
    pub(crate) options: Vec<String>,
    pub(crate) selected: usize,
}

impl WizardChoiceModal {
    /// Construct with a list of labels and `0` highlighted by default.
    /// Panics if `options` is empty — that's a programming error
    /// (a zero-option choice is meaningless).
    pub(crate) fn new(title: impl Into<String>, options: Vec<String>) -> Self {
        assert!(!options.is_empty(), "WizardChoiceModal requires at least one option");
        Self {
            title: title.into(),
            options,
            selected: 0,
        }
    }

    /// Outcome of `handle_key`.
    #[allow(dead_code)] // Wired in v2.2.18 alongside wizard migration.
    pub(crate) fn handle_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> ChoiceAction {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                ChoiceAction::Continue
            }
            KeyCode::Down => {
                if self.selected + 1 < self.options.len() {
                    self.selected += 1;
                }
                ChoiceAction::Continue
            }
            KeyCode::Enter => ChoiceAction::Committed(self.selected),
            KeyCode::Esc => ChoiceAction::Cancelled,
            KeyCode::Char(c) if c.is_ascii_digit() => {
                // Number keys: 1-based to match the wizard's `[1]/[2]/[3]/[4]`
                // numbering. `0` is ignored.
                let n = c.to_digit(10).unwrap_or(0) as usize;
                if n >= 1 && n <= self.options.len() {
                    self.selected = n - 1;
                    ChoiceAction::Committed(self.selected)
                } else {
                    ChoiceAction::Continue
                }
            }
            _ => ChoiceAction::Continue,
        }
    }
}

/// Outcome of `WizardChoiceModal::handle_key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Wired in v2.2.18 alongside wizard migration.
pub(crate) enum ChoiceAction {
    /// Stay open, redraw.
    Continue,
    /// Resolved with the selected index (0-based).
    Committed(usize),
    /// Esc pressed — wizard should abort the current chain.
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // ─── ModalQueue ────────────────────────────────────────────────

    #[test]
    fn queue_drains_in_fifo_order() {
        let mut q = ModalQueue::new();
        q.push(QueuedModal::Choice {
            tag: "step7-layout".to_string(),
            modal: WizardChoiceModal::new(
                "Layout",
                vec!["Vertical".into(), "Classic".into()],
            ),
        });
        q.push(QueuedModal::Confirm {
            tag: "step8-mouse".to_string(),
            modal: ConfirmModal::new("Mouse capture", "Enable mouse capture?"),
        });
        assert!(!q.is_empty());
        assert_eq!(q.front().unwrap().tag(), "step7-layout");
        q.resolve_front(ModalAnswer::Choice(0));
        assert_eq!(q.front().unwrap().tag(), "step8-mouse");
        q.resolve_front(ModalAnswer::Confirm(ConfirmChoice::No));
        assert!(q.is_empty());
    }

    #[test]
    fn queue_records_answers_by_tag() {
        let mut q = ModalQueue::new();
        q.push(QueuedModal::Choice {
            tag: "layout".to_string(),
            modal: WizardChoiceModal::new("L", vec!["A".into(), "B".into()]),
        });
        q.resolve_front(ModalAnswer::Choice(1));
        assert_eq!(q.answer_for("layout"), Some(&ModalAnswer::Choice(1)));
        assert_eq!(q.answer_for("nonexistent"), None);
    }

    #[test]
    fn queue_resolve_front_on_empty_returns_none() {
        let mut q = ModalQueue::new();
        assert!(q.resolve_front(ModalAnswer::ChoiceCancelled).is_none());
    }

    // ─── WizardChoiceModal ─────────────────────────────────────────

    #[test]
    fn choice_modal_arrow_keys_navigate() {
        let mut m = WizardChoiceModal::new("t", vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(m.selected, 0);
        assert_eq!(m.handle_key(key(KeyCode::Down)), ChoiceAction::Continue);
        assert_eq!(m.selected, 1);
        assert_eq!(m.handle_key(key(KeyCode::Down)), ChoiceAction::Continue);
        assert_eq!(m.selected, 2);
        // Bound: Down at the bottom stays.
        assert_eq!(m.handle_key(key(KeyCode::Down)), ChoiceAction::Continue);
        assert_eq!(m.selected, 2);
        // Up walks back.
        assert_eq!(m.handle_key(key(KeyCode::Up)), ChoiceAction::Continue);
        assert_eq!(m.selected, 1);
    }

    #[test]
    fn choice_modal_enter_commits() {
        let mut m = WizardChoiceModal::new("t", vec!["x".into(), "y".into()]);
        m.handle_key(key(KeyCode::Down));
        assert_eq!(m.handle_key(key(KeyCode::Enter)), ChoiceAction::Committed(1));
    }

    #[test]
    fn choice_modal_digit_jumps_and_commits() {
        let mut m = WizardChoiceModal::new(
            "t",
            vec!["a".into(), "b".into(), "c".into(), "d".into()],
        );
        // `3` jumps to and commits the 3rd option (index 2).
        assert_eq!(m.handle_key(key(KeyCode::Char('3'))), ChoiceAction::Committed(2));
        assert_eq!(m.selected, 2);
        // Out-of-range digit is ignored.
        assert_eq!(m.handle_key(key(KeyCode::Char('9'))), ChoiceAction::Continue);
    }

    #[test]
    fn choice_modal_esc_cancels() {
        let mut m = WizardChoiceModal::new("t", vec!["a".into(), "b".into()]);
        assert_eq!(m.handle_key(key(KeyCode::Esc)), ChoiceAction::Cancelled);
    }

    #[test]
    #[should_panic(expected = "at least one option")]
    fn choice_modal_panics_on_zero_options() {
        let _ = WizardChoiceModal::new("t", vec![]);
    }
}
