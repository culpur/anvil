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
//!
//! v2.2.18 (task #666, Agent A1): rich `Choice` rows on
//! `WizardChoiceModal` (badge + description), a `StreamingOutputModal`
//! primitive in `super::streaming`, and a `HealthProbeModal` primitive
//! in `super::health_probe`. The streaming modal is NOT queue-able
//! (subprocess lifecycle is bound to a single call into
//! `WizardModalRunner::run_streaming_output`); the health-probe modal
//! is queue-able via `QueuedModal::HealthProbe`.

use std::collections::VecDeque;

use super::confirm::{ConfirmModal, ConfirmChoice};
use super::health_probe::HealthProbeModal;
use super::password::PasswordModal;
use super::text_input::TextInputModal;
use super::textarea::TextareaModal;

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
    /// A single-line free-text input with optional default (task #642).
    /// Used by the v2.2.17 wizard for "Ollama URL" and "Profile name".
    TextInput {
        tag: String,
        modal: TextInputModal,
    },
    /// v2.2.18 task #666: a multi-issue health-probe checklist with
    /// spacebar-toggle repair flags. Renders the detected-issues list
    /// with status icons (✓ ✗ ⚠) and lets the user pick which to
    /// repair. Resolves to `ModalAnswer::HealthCheck`.
    #[allow(dead_code)]
    HealthProbe {
        tag: String,
        modal: HealthProbeModal,
    },
    /// task #684: multi-line textarea for long-description fields.
    /// Enter inserts a newline; Ctrl+Enter submits. Resolves to
    /// `ModalAnswer::TextareaInput(String)`.
    #[allow(dead_code)]
    TextareaInput {
        tag: String,
        modal: TextareaModal,
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
            Self::TextInput { tag, .. } => tag,
            Self::HealthProbe { tag, .. } => tag,
            Self::TextareaInput { tag, .. } => tag,
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
    /// The secret submitted by a password modal — kept ONLY in the
    /// wizard's per-step capture path; never persisted on `ModalQueue`.
    /// The queue runner converts a successful password submit to
    /// `PasswordSubmitted(true)` before recording; this variant is the
    /// in-flight value the wizard reads on the call-site that needs the
    /// raw secret (e.g. vault setup).  Always zeroized by ownership
    /// transfer at the consumer.
    #[allow(dead_code)]
    PasswordValue(String),
    /// Selected index in the choice list (0-based). `Esc` records
    /// `ChoiceCancelled` so callers can detect a wizard abort.
    Choice(usize),
    ChoiceCancelled,
    /// Free-text submission from a `TextInputModal`. Carries the value
    /// (which may be the user's default-fallback string when the buffer
    /// was empty on Enter / Esc).
    TextInput(String),
    /// `Ctrl+C` on a TextInputModal — caller should abort the step.
    #[allow(dead_code)]
    TextInputCancelled,
    /// v2.2.18 task #666: outcome of a `StreamingOutputModal`. The
    /// subprocess exited with `exit_code` and the modal captured the
    /// last N lines of its merged stdout/stderr in `output_tail` (in
    /// arrival order, oldest first). `exit_code = -1` means the user
    /// cancelled via Esc → confirm → SIGTERM/SIGKILL.
    #[allow(dead_code)]
    StreamingResult {
        exit_code: i32,
        output_tail: Vec<String>,
    },
    /// v2.2.18 task #666: outcome of a `HealthProbeModal`. `repair`
    /// lists the indices the user marked for repair (via `[r]`/`[a]`);
    /// `quit` is `true` only if the user pressed `q`/Esc (caller
    /// should treat as wizard-abort). Continue-without-repair is
    /// signaled by `repair.is_empty() && !quit`.
    #[allow(dead_code)]
    HealthCheck {
        repair: Vec<usize>,
        quit: bool,
    },
    /// task #684: multi-line text submitted via a `TextareaModal`.
    /// Carries the full buffer contents.
    #[allow(dead_code)]
    TextareaInput(String),
    /// task #684: Esc/Ctrl+C on a `TextareaModal` — caller should
    /// abort the wizard step.
    #[allow(dead_code)]
    TextareaInputCancelled,
    /// Task #767: Ctrl+B pressed on a Choice or Confirm modal. The
    /// wizard orchestrator's per-step caller should treat this as a
    /// signal to re-run the previous step. Caller must decide whether
    /// to honor it (cheap steps) or fall through to default
    /// (irreversible steps).
    #[allow(dead_code)]
    GoBack,
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

/// A single row in a `WizardChoiceModal`.
///
/// v2.2.18 (task #666): adds the optional `badge` + `description`
/// fields so the wizard can render the 4-state install pattern:
///
/// ```text
///   [1] Install Ollama now            [recommended]
///       Full installer + benchmark, ~5min
///   [2] I have it elsewhere
///       Point Anvil at an existing Ollama URL
/// ```
///
/// Backward-compatible: bare labels (the legacy
/// `WizardChoiceModal::new(title, vec!["A".into(), "B".into()])` API)
/// still produce a flat list with no badges or descriptions because
/// `From<S: AsRef<str>>` is implemented for `Choice`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Choice {
    pub(crate) label: String,
    pub(crate) badge: Option<String>,
    pub(crate) description: Option<String>,
}

impl Choice {
    /// New row with just a label.
    #[allow(dead_code)]
    pub(crate) fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            badge: None,
            description: None,
        }
    }

    /// Builder: attach a short badge rendered to the right of the
    /// label (e.g. `[recommended]`, `[detected]`). The modal wraps it
    /// in square brackets, so callers pass the bare text.
    #[allow(dead_code)]
    pub(crate) fn with_badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    /// Builder: attach a one-line description rendered under the label
    /// in the modal-secondary color. Pass a single line; the modal
    /// does not wrap.
    #[allow(dead_code)]
    pub(crate) fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

impl<S: AsRef<str>> From<S> for Choice {
    fn from(s: S) -> Self {
        Choice {
            label: s.as_ref().to_string(),
            badge: None,
            description: None,
        }
    }
}

/// A numbered-list choice modal — the wizard's "Step 7: TUI Layout"
/// pattern (`[1] Vertical Split [2] Classic [3] Three-Pane [4]
/// Journal`). Up/Down navigate, Enter commits, Esc cancels.
///
/// The rendered overlay is intentionally minimal; the wizard adapter
/// supplies the title + option labels and reads the selected index
/// back from `ModalAnswer::Choice(idx)`.
///
/// v2.2.18 (task #666): each row may carry a badge + description, and
/// the modal may carry a custom footer hint. The legacy `new(title,
/// vec![label, label])` constructor is preserved verbatim.
#[derive(Debug, Clone)]
pub(crate) struct WizardChoiceModal {
    pub(crate) title: String,
    pub(crate) options: Vec<Choice>,
    pub(crate) selected: usize,
    /// Optional footer help line. `None` falls back to the default
    /// hint ("↑↓ navigate · Enter select · 1-9 jump · Esc cancel").
    pub(crate) footer_hint: Option<String>,
}

impl WizardChoiceModal {
    /// Legacy constructor: title + list of label strings.
    /// Equivalent to
    /// `WizardChoiceModal::new_titled(title).with_choices(vec![Choice::new(l), ...])`.
    /// Panics on an empty `options` vec — a zero-option choice is meaningless.
    ///
    /// Signature preserved verbatim from v2.2.17 so every existing
    /// caller compiles without change. The new rich form is
    /// `WizardChoiceModal::new_titled(...).with_choices(...)`.
    pub(crate) fn new(title: impl Into<String>, options: Vec<String>) -> Self {
        assert!(
            !options.is_empty(),
            "WizardChoiceModal requires at least one option"
        );
        Self {
            title: title.into(),
            options: options.into_iter().map(Choice::from).collect(),
            selected: 0,
            footer_hint: None,
        }
    }

    /// v2.2.18 (task #666) rich constructor: start with just a title,
    /// then chain `.with_choices(...)` + optional `.with_footer_hint(...)`.
    /// The modal is unusable until `with_choices` is called — calling
    /// `render` on an empty modal early-returns; `handle_key` returns
    /// `Continue` for navigation and `Cancelled` for Esc.
    #[allow(dead_code)]
    pub(crate) fn new_titled(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            options: Vec::new(),
            selected: 0,
            footer_hint: None,
        }
    }

    /// Builder: replace the options list with the supplied `Choice`
    /// rows. Panics on an empty vec — matches `new()`'s contract.
    #[allow(dead_code)]
    pub(crate) fn with_choices(mut self, choices: Vec<Choice>) -> Self {
        assert!(
            !choices.is_empty(),
            "WizardChoiceModal requires at least one choice"
        );
        self.options = choices;
        self.selected = 0;
        self
    }

    /// Builder: override the default footer hint. Pass an empty string
    /// to suppress the footer entirely.
    #[allow(dead_code)]
    pub(crate) fn with_footer_hint(mut self, hint: impl Into<String>) -> Self {
        self.footer_hint = Some(hint.into());
        self
    }

    /// Builder: pre-select the row at `index` so the modal opens on
    /// the user's current value rather than always on row 0.  Added
    /// for Phase A6 (task #645): the wizard's language picker opens
    /// on the persisted locale so re-running the wizard does not
    /// silently reset the language to "en" if the user just hits
    /// Enter.  Out-of-range values clamp to 0.
    #[allow(dead_code)]
    pub(crate) fn with_default_index(mut self, index: usize) -> Self {
        self.selected = if index < self.options.len().max(1) {
            index
        } else {
            0
        };
        self
    }

    /// Outcome of `handle_key`.
    pub(crate) fn handle_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> ChoiceAction {
        use crossterm::event::{KeyCode, KeyModifiers};
        if self.options.is_empty() {
            return match key.code {
                KeyCode::Esc => ChoiceAction::Cancelled,
                _ => ChoiceAction::Continue,
            };
        }
        // Task #767: Ctrl+B is the global Back keybind across all
        // wizard modals. We don't use Shift+Tab/BackTab because
        // ConfirmModal already uses Tab/BackTab for Yes/No toggle;
        // Ctrl+B has no existing binding in the wizard modal flow.
        if key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return ChoiceAction::GoBack;
        }
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

    /// Task #579 + #666: render the choice modal as a centered overlay.
    /// Used by the wizard's standalone runner (`tui::wizard_runner`) and
    /// any future in-TUI caller that drains the queue from inside an
    /// alt-screen. Mirrors the visual contract of `ConfirmModal::render`
    /// (rounded border, accent-colored title, dim-numbered options, the
    /// `selected` index drawn with reverse video) and additionally
    /// renders per-row badges + descriptions when present.
    pub(crate) fn render(
        &self,
        frame: &mut ratatui::Frame,
        area: ratatui::layout::Rect,
        accent: ratatui::style::Color,
    ) {
        use ratatui::layout::Rect;
        use ratatui::style::{Color, Modifier, Style};
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

        if self.options.is_empty() {
            return;
        }

        let modal_w = area.width.saturating_sub(8).min(80).max(30);

        // Row count: 1 line per option + 1 extra for each option that
        // carries a non-empty description. Then 2 padding + 1 footer +
        // 2 border = N + 5.
        let desc_rows: u16 = self
            .options
            .iter()
            .filter(|c| c.description.as_deref().map(|s| !s.is_empty()).unwrap_or(false))
            .count() as u16;
        let opts_rows = self.options.len() as u16 + desc_rows;
        let needed_h: u16 = opts_rows + 5;
        if area.width < 12 || area.height < needed_h.min(7) {
            return;
        }
        let modal_h = needed_h.min(area.height.saturating_sub(2));
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

        let mut lines: Vec<Line<'static>> =
            Vec::with_capacity(self.options.len() + desc_rows as usize + 3);
        lines.push(Line::from(""));
        for (idx, opt) in self.options.iter().enumerate() {
            let prefix = format!("  [{}] ", idx + 1);
            let label_style = if idx == self.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(3);
            spans.push(Span::styled(format!("{prefix}{}", opt.label), label_style));
            if let Some(badge) = opt.badge.as_ref().filter(|b| !b.is_empty()) {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    format!("[{badge}]"),
                    Style::default()
                        .fg(super::modal_secondary_color())
                        .add_modifier(Modifier::ITALIC),
                ));
            }
            lines.push(Line::from(spans));

            if let Some(desc) = opt.description.as_ref().filter(|d| !d.is_empty()) {
                lines.push(Line::from(Span::styled(
                    format!("      {desc}"),
                    Style::default().fg(super::modal_secondary_color()),
                )));
            }
        }
        lines.push(Line::from(""));
        let footer = self
            .footer_hint
            .as_deref()
            .unwrap_or("↑↓ navigate · Enter select · 1-9 jump · Ctrl+B back · Esc cancel");
        if !footer.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {footer}"),
                Style::default().fg(super::modal_secondary_color()),
            )));
        }

        frame.render_widget(Paragraph::new(lines), inner);
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
    /// Shift+Tab pressed (task #767): wizard should re-run the previous
    /// step. The orchestrator pops the history stack and re-enters.
    GoBack,
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
                vec!["Vertical".to_string(), "Classic".to_string()],
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
            modal: WizardChoiceModal::new(
                "L",
                vec!["A".to_string(), "B".to_string()],
            ),
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

    // ─── WizardChoiceModal (legacy v2.2.17 API) ────────────────────

    #[test]
    fn choice_modal_arrow_keys_navigate() {
        let mut m = WizardChoiceModal::new(
            "t",
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
        );
        assert_eq!(m.selected, 0);
        assert_eq!(m.handle_key(key(KeyCode::Down)), ChoiceAction::Continue);
        assert_eq!(m.selected, 1);
        assert_eq!(m.handle_key(key(KeyCode::Down)), ChoiceAction::Continue);
        assert_eq!(m.selected, 2);
        assert_eq!(m.handle_key(key(KeyCode::Down)), ChoiceAction::Continue);
        assert_eq!(m.selected, 2);
        assert_eq!(m.handle_key(key(KeyCode::Up)), ChoiceAction::Continue);
        assert_eq!(m.selected, 1);
    }

    #[test]
    fn choice_modal_enter_commits() {
        let mut m = WizardChoiceModal::new(
            "t",
            vec!["x".to_string(), "y".to_string()],
        );
        m.handle_key(key(KeyCode::Down));
        assert_eq!(
            m.handle_key(key(KeyCode::Enter)),
            ChoiceAction::Committed(1)
        );
    }

    #[test]
    fn choice_modal_digit_jumps_and_commits() {
        let mut m = WizardChoiceModal::new(
            "t",
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
            ],
        );
        assert_eq!(
            m.handle_key(key(KeyCode::Char('3'))),
            ChoiceAction::Committed(2)
        );
        assert_eq!(m.selected, 2);
        assert_eq!(
            m.handle_key(key(KeyCode::Char('9'))),
            ChoiceAction::Continue
        );
    }

    #[test]
    fn choice_modal_esc_cancels() {
        let mut m = WizardChoiceModal::new(
            "t",
            vec!["a".to_string(), "b".to_string()],
        );
        assert_eq!(m.handle_key(key(KeyCode::Esc)), ChoiceAction::Cancelled);
    }

    #[test]
    #[should_panic(expected = "at least one option")]
    fn choice_modal_panics_on_zero_options() {
        let _ = WizardChoiceModal::new("t", Vec::<String>::new());
    }

    // ─── v2.2.18 task #666: rich Choice rows ───────────────────────

    #[test]
    fn rich_choice_modal_builder_chain() {
        let m = WizardChoiceModal::new_titled("Install Ollama?")
            .with_choices(vec![
                Choice::new("Install Ollama now")
                    .with_badge("recommended")
                    .with_description("Full installer + benchmark, ~5min"),
                Choice::new("I have it elsewhere")
                    .with_description("Point Anvil at an existing Ollama URL"),
                Choice::new("Skip — don't ask again"),
                Choice::new("Maybe later"),
            ])
            .with_footer_hint("press 1-4, or ↑/↓ + Enter");
        assert_eq!(m.options.len(), 4);
        assert_eq!(m.options[0].badge.as_deref(), Some("recommended"));
        assert_eq!(
            m.options[0].description.as_deref(),
            Some("Full installer + benchmark, ~5min"),
        );
        assert!(m.options[2].badge.is_none());
        assert_eq!(
            m.footer_hint.as_deref(),
            Some("press 1-4, or ↑/↓ + Enter")
        );
        assert_eq!(m.selected, 0);
    }

    #[test]
    fn rich_choice_modal_handles_digit_and_arrows() {
        let mut m = WizardChoiceModal::new_titled("Pick").with_choices(vec![
            Choice::new("one").with_badge("recommended"),
            Choice::new("two"),
            Choice::new("three").with_description("desc"),
            Choice::new("four"),
        ]);
        assert_eq!(
            m.handle_key(key(KeyCode::Char('3'))),
            ChoiceAction::Committed(2)
        );
        assert_eq!(m.selected, 2);
        let mut m = WizardChoiceModal::new_titled("Pick").with_choices(vec![
            Choice::new("one"),
            Choice::new("two"),
        ]);
        m.handle_key(key(KeyCode::Down));
        assert_eq!(
            m.handle_key(key(KeyCode::Enter)),
            ChoiceAction::Committed(1)
        );
    }

    #[test]
    fn rich_choice_modal_render_emits_badge_and_description() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::style::Color;

        let backend = TestBackend::new(80, 20);
        let mut term = Terminal::new(backend).unwrap();
        let m = WizardChoiceModal::new_titled("Install?").with_choices(vec![
            Choice::new("Install now")
                .with_badge("recommended")
                .with_description("Full installer"),
            Choice::new("Skip"),
        ]);
        term.draw(|f| {
            m.render(f, f.area(), Color::Cyan);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let dump: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            dump.contains("[recommended]"),
            "badge missing from render: {dump:?}"
        );
        assert!(
            dump.contains("Full installer"),
            "description missing from render: {dump:?}"
        );
    }

    #[test]
    fn legacy_choice_constructor_still_works() {
        // Bare-string constructor must continue to work — this is
        // the back-compat guarantee for every existing caller that
        // has not migrated to the rich `Choice` form.
        let mut m = WizardChoiceModal::new(
            "Theme",
            vec!["Dark".to_string(), "Light".to_string()],
        );
        assert_eq!(m.options.len(), 2);
        assert!(m.options[0].badge.is_none());
        assert!(m.options[0].description.is_none());
        assert_eq!(
            m.handle_key(key(KeyCode::Char('2'))),
            ChoiceAction::Committed(1)
        );
    }
}
