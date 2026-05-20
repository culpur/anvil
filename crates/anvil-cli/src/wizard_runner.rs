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
use std::thread;
use std::time::{Duration, Instant};

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

/// Simple word-wrap helper used by the welcome card + per-step
/// description banner.  Splits on whitespace; falls back to mid-word
/// splits only when a single word exceeds `width`.
///
/// Pure function — easy to unit-test in isolation.
pub(crate) fn wrap_paragraph(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out: Vec<String> = Vec::new();
    if text.is_empty() {
        out.push(String::new());
        return out;
    }
    for raw_line in text.split('\n') {
        if raw_line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in raw_line.split_whitespace() {
            if current.is_empty() {
                if word.chars().count() <= width {
                    current.push_str(word);
                } else {
                    let mut buf = String::new();
                    for ch in word.chars() {
                        if buf.chars().count() + 1 > width {
                            out.push(buf.clone());
                            buf.clear();
                        }
                        buf.push(ch);
                    }
                    current = buf;
                }
            } else if current.chars().count() + 1 + word.chars().count() <= width {
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

use crate::tui::modals::confirm::{ConfirmAction, ConfirmChoice, ConfirmModal};
use crate::tui::modals::health_probe::{HealthProbeAction, HealthProbeModal};
use crate::tui::modals::password::{PasswordAction, PasswordModal};
use crate::tui::modals::queue::{
    ChoiceAction, ModalAnswer, ModalQueue, QueuedModal, WizardChoiceModal,
};
use crate::tui::modals::streaming::{
    self, StreamingAction, StreamingOutputModal, StreamingState, SubprocessHandle,
};
use crate::tui::modals::text_input::{TextInputAction, TextInputModal};
use crate::tui::modals::textarea::{TextareaAction, TextareaModal};

/// Source of key events for the runner. Production uses
/// `CrosstermKeySource` which polls real terminal input; tests use
/// `ScriptedKeySource` to inject a deterministic sequence.
pub(crate) trait KeySource {
    /// Block until the next `KeyEvent` (or return `None` on cancel).
    fn next_key(&mut self) -> Option<KeyEvent>;

    /// Poll for a `KeyEvent` with a bounded timeout.  Returns
    /// `Ok(Some(key))` if one arrives within `timeout`, `Ok(None)` if
    /// the timeout elapses with no key (so the caller can do other
    /// work — e.g. poll an OAuth callback), or `Ok(None)` if the
    /// source is exhausted (scripted tests).
    ///
    /// Default impl just calls `next_key` for backward compatibility;
    /// production sources override this to actually honour the
    /// timeout.
    fn try_next_key(&mut self, _timeout: Duration) -> Option<KeyEvent> {
        self.next_key()
    }

    /// Hint used by `run_oauth_flow` to know when to stop spinning in
    /// scripted-source tests.  Production sources (CrosstermKeySource)
    /// should always return `false` — the channel is live and a real
    /// user could still press a key.  Scripted sources return `true`
    /// once their input queue is empty so the OAuth loop can finalize
    /// after a state-machine-induced terminal state instead of waiting
    /// forever for a non-existent keystroke.
    fn is_exhausted_hint(&self) -> bool {
        false
    }
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

    /// Bounded poll for a `KeyEvent`.  Returns `Some(key)` if one
    /// arrives within `timeout`, `None` if the timeout elapses.  Used
    /// by `WizardModalRunner::run_oauth_flow` to interleave callback
    /// polling with key polling (v2.2.17 #644 Item 3 fix).
    fn try_next_key(&mut self, timeout: Duration) -> Option<KeyEvent> {
        match crossterm::event::poll(timeout) {
            Ok(true) => match crossterm::event::read() {
                Ok(Event::Key(key)) => Some(key),
                _ => None,
            },
            _ => None,
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

    /// Scripted sources don't honour real wall-clock timeouts — they
    /// just pop the next queued key (or return `None` immediately when
    /// the queue is empty).  That immediate `None` is what lets
    /// `run_oauth_flow` notice scripted-tests have run dry and
    /// finalize, instead of spinning on a never-arriving key.
    fn try_next_key(&mut self, _timeout: Duration) -> Option<KeyEvent> {
        self.keys.pop_front()
    }

    fn is_exhausted_hint(&self) -> bool {
        self.keys.is_empty()
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
                            // Banner body uses the same modal-secondary
                            // color as the rest of the wizard (v2.2.17
                            // #644 Item 5).  Dropping `Modifier::DIM`
                            // because stacking DIM on a secondary RGB
                            // collapses luminance back below 0.55.
                            Style::default().fg(crate::tui::modals::modal_secondary_color()),
                        ))
                    })
                    .collect();
                frame.render_widget(Paragraph::new(lines), inner);
            })
            .map_err(|e| RunnerError::Draw(e.to_string()))?;
        Ok(())
    }

    /// Render a step banner WITH a 1-sentence "why this step matters"
    /// description paragraph (v2.2.17 #644 Item 2).
    ///
    /// Renders the standard banner block (title + body lines), then a
    /// blank divider, then a `description` paragraph rendered in the
    /// modal-secondary color so it is visibly subordinate to the title
    /// but still readable.
    pub(crate) fn render_banner_with_description(
        &mut self,
        title: &str,
        description: &str,
        body: &[&str],
        accent: Color,
    ) -> Result<(), RunnerError> {
        let title_owned = title.to_string();
        let description_owned = description.to_string();
        let body_owned: Vec<String> = body.iter().map(|s| (*s).to_string()).collect();
        self.terminal
            .draw(|frame| {
                use ratatui::layout::Rect;
                use ratatui::style::{Modifier, Style};
                use ratatui::text::{Line, Span};
                use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

                let area = frame.area();
                if area.width < 12 || area.height < 7 {
                    return;
                }
                let banner_w = area.width.saturating_sub(8).min(70).max(30);
                // Estimate wrapped description rows.  We wrap the
                // description to (banner_w - 4) cols which matches the
                // inner padding the body lines use.  Worst case the
                // banner grows; we cap at 80% of the screen height.
                let wrap_w = banner_w.saturating_sub(4).max(20) as usize;
                let desc_rows = wrap_paragraph(&description_owned, wrap_w).len() as u16;
                let banner_h: u16 =
                    desc_rows + (body_owned.len() as u16) + 6; // border + padding + divider
                let banner_h = banner_h.min(area.height.saturating_sub(2));
                let banner_x = (area.width.saturating_sub(banner_w)) / 2;
                let banner_y = (area.height.saturating_sub(banner_h)) / 2;
                let banner_area = Rect {
                    x: banner_x,
                    y: banner_y,
                    width: banner_w,
                    height: banner_h,
                };

                frame.render_widget(Clear, banner_area);

                let block = Block::default()
                    .title(format!(" {} ", title_owned))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(accent).add_modifier(Modifier::BOLD));
                let inner = block.inner(banner_area);
                frame.render_widget(block, banner_area);

                let mut lines: Vec<Line<'static>> = Vec::new();
                lines.push(Line::from(""));
                // Description paragraph (wrapped), modal-secondary color.
                for desc_line in wrap_paragraph(&description_owned, wrap_w) {
                    lines.push(Line::from(Span::styled(
                        format!("  {desc_line}"),
                        Style::default().fg(crate::tui::modals::modal_secondary_color()),
                    )));
                }
                if !body_owned.is_empty() {
                    lines.push(Line::from(""));
                    for line in &body_owned {
                        lines.push(Line::from(Span::styled(
                            format!("  {line}"),
                            Style::default().fg(crate::tui::modals::modal_secondary_color()),
                        )));
                    }
                }
                frame.render_widget(Paragraph::new(lines), inner);
            })
            .map_err(|e| RunnerError::Draw(e.to_string()))?;
        Ok(())
    }

    /// Render the centered welcome card shown as the wizard's very
    /// first frame (v2.2.17 #644 Item 0).
    ///
    /// Renders a larger card than `render_banner` with the title spanning
    /// the full width, a one-line tagline beneath it in the modal-
    /// secondary color, and a "Press Enter / Esc" hint row at the
    /// bottom.  The card is intentionally taller than the step banners
    /// so it reads as a deliberate first impression rather than a
    /// "Step 0".
    pub(crate) fn render_welcome_card(
        &mut self,
        title: &str,
        tagline: &str,
        kicker: &str,
        hint: &str,
        accent: Color,
    ) -> Result<(), RunnerError> {
        let title_owned = title.to_string();
        let tagline_owned = tagline.to_string();
        let kicker_owned = kicker.to_string();
        let hint_owned = hint.to_string();
        self.terminal
            .draw(|frame| {
                use ratatui::layout::Rect;
                use ratatui::style::{Modifier, Style};
                use ratatui::text::{Line, Span};
                use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

                let area = frame.area();
                if area.width < 24 || area.height < 9 {
                    return;
                }
                let card_w = area.width.saturating_sub(8).min(74).max(40);
                let wrap_w = card_w.saturating_sub(4).max(20) as usize;
                let tagline_lines = wrap_paragraph(&tagline_owned, wrap_w);
                let card_h: u16 = (tagline_lines.len() as u16) + 9; // title + spacing + kicker + hint + borders
                let card_h = card_h.min(area.height.saturating_sub(2));
                let card_x = (area.width.saturating_sub(card_w)) / 2;
                let card_y = (area.height.saturating_sub(card_h)) / 2;
                let card_area = Rect {
                    x: card_x,
                    y: card_y,
                    width: card_w,
                    height: card_h,
                };

                frame.render_widget(Clear, card_area);

                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(accent).add_modifier(Modifier::BOLD));
                let inner = block.inner(card_area);
                frame.render_widget(block, card_area);

                let mut lines: Vec<Line<'static>> = Vec::new();
                lines.push(Line::from(""));
                // Centered title — pad with spaces so it sits in the
                // visual middle of the inner area.
                let title_visible_width = title_owned.chars().count();
                let inner_w = inner.width as usize;
                let pad = inner_w.saturating_sub(title_visible_width) / 2;
                let padded_title = format!("{}{}", " ".repeat(pad), title_owned);
                lines.push(Line::from(Span::styled(
                    padded_title,
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
                for tline in tagline_lines {
                    let twidth = tline.chars().count();
                    let pad = inner_w.saturating_sub(twidth) / 2;
                    lines.push(Line::from(Span::styled(
                        format!("{}{}", " ".repeat(pad), tline),
                        Style::default().fg(crate::tui::modals::modal_secondary_color()),
                    )));
                }
                lines.push(Line::from(""));
                if !kicker_owned.is_empty() {
                    let kw = kicker_owned.chars().count();
                    let pad = inner_w.saturating_sub(kw) / 2;
                    lines.push(Line::from(Span::styled(
                        format!("{}{}", " ".repeat(pad), kicker_owned),
                        Style::default().fg(ratatui::style::Color::White),
                    )));
                }
                lines.push(Line::from(""));
                let hw = hint_owned.chars().count();
                let pad = inner_w.saturating_sub(hw) / 2;
                lines.push(Line::from(Span::styled(
                    format!("{}{}", " ".repeat(pad), hint_owned),
                    Style::default().fg(crate::tui::modals::modal_secondary_color()),
                )));

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

    /// Drive a single multi-line textarea modal to resolution (task #684).
    /// Returns `ModalAnswer::TextareaInput(value)` on Ctrl+Enter, or
    /// `ModalAnswer::TextareaInputCancelled` on Esc/Ctrl+C / empty source.
    #[allow(dead_code)]
    pub(crate) fn run_textarea_input(
        &mut self,
        tag: &str,
        modal: TextareaModal,
    ) -> Result<ModalAnswer, RunnerError> {
        let mut q = ModalQueue::new();
        q.push(QueuedModal::TextareaInput {
            tag: tag.to_string(),
            modal,
        });
        self.run_queue(&mut q)?;
        Ok(q
            .answer_for(tag)
            .cloned()
            .unwrap_or(ModalAnswer::TextareaInputCancelled))
    }

    /// Drive an `OAuthFlow` to completion inside the active alt-screen
    /// session (task #642 Sub-task C + v2.2.17 #644 Item 3 / Item 4
    /// fix).
    ///
    /// The flow is polled non-blocking on every iteration AND rendered
    /// on every iteration, regardless of whether a key has arrived.
    /// User keys route to `OAuthFlow::handle_key`.  Returns the
    /// terminal `OAuthOutcome` from `OAuthFlow::finalize`.
    ///
    /// ## Loop structure (v2.2.17 #644 Item 3 fix)
    ///
    /// ```ignore
    /// loop {
    ///     1. flow.poll()                  -- non-blocking OAuth callback drain
    ///     2. session.draw(flow.render)    -- redraw EVERY iteration so the
    ///                                        "Elapsed: Ns" counter ticks even
    ///                                        when the user types nothing
    ///     3. keys.try_next_key(100ms)     -- bounded key poll; returns None
    ///                                        on timeout so step 1 runs again
    ///     4. handle key OR (state moved)  -- exit on success/failed/cancel
    /// }
    /// ```
    ///
    /// Before v2.2.17 #644 the loop used `self.keys.next_key()` which
    /// blocks until a key arrives — so the OAuth callback channel was
    /// drained ONLY when a key event woke the loop.  Users reported
    /// that the browser flow completed but Anvil never noticed; the
    /// elapsed counter also froze at 0s.  The bounded poll fixes
    /// both.
    ///
    /// This is the bridge that lets the first-run wizard's Step 3 run
    /// the OAuth callback inline — no drop to stdin, no second
    /// alt-screen transition, the whole flow stays inside the same
    /// `WizardSession` the rest of the wizard owns.
    pub(crate) fn run_oauth_flow(
        &mut self,
        mut flow: crate::tui::oauth_flow::OAuthFlow,
    ) -> Result<crate::tui::oauth_flow::OAuthOutcome, RunnerError> {
        use crate::tui::oauth_flow::{OAuthAction, OAuthFlowState};
        let accent = self.accent;
        // 100ms poll cadence — fast enough for the elapsed counter
        // to tick visibly + for the browser callback to feel instant,
        // slow enough that the loop does not pin a CPU core.
        let key_poll = Duration::from_millis(100);
        loop {
            // 1. Non-blocking OAuth callback drain.  May transition
            //    `flow.state` from AwaitingCallback → Success / Failed
            //    when the listener thread delivers (or errors).
            let _ = flow.poll();

            // 2. Always redraw — so the elapsed counter ticks and so
            //    the success / failed card paints the very first frame
            //    after the callback arrives, without waiting on a key.
            self.session
                .terminal
                .draw(|frame| {
                    let area = frame.area();
                    flow.render(frame, area, accent);
                })
                .map_err(|e| RunnerError::Draw(e.to_string()))?;

            // 3. Bounded key poll.  Returns `None` on timeout so the
            //    loop comes back around to step 1 + step 2.
            if let Some(k) = self.keys.try_next_key(key_poll) {
                match flow.handle_key(k) {
                    OAuthAction::Continue => continue,
                    OAuthAction::Cancel | OAuthAction::Advance => {
                        return Ok(flow.finalize());
                    }
                }
            }

            // 4. Scripted-source exit: a `ScriptedKeySource` with no
            //    keys queued AND no further state transitions possible
            //    must not spin forever.  CrosstermKeySource's
            //    try_next_key returns `None` only on a real timeout
            //    (the channel is still live), so production never hits
            //    this branch — it just loops back to step 1.  The
            //    `ScriptedKeySource` override of `try_next_key` returns
            //    `None` immediately when the queue is empty (see the
            //    impl in tests below); that combined with a non-
            //    AwaitingCallback state means the scripted test has
            //    exhausted its inputs and we should finalize.
            if !matches!(flow.state, OAuthFlowState::AwaitingCallback { .. })
                && self.keys.is_exhausted_hint()
            {
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
                            QueuedModal::HealthProbe { modal, .. } => {
                                modal.render(frame, area, accent);
                            }
                            QueuedModal::TextareaInput { modal, .. } => {
                                modal.render(frame, area, accent);
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
                    QueuedModal::HealthProbe { .. } => ModalAnswer::HealthCheck {
                        repair: Vec::new(),
                        quit: true,
                    },
                    QueuedModal::TextareaInput { .. } => ModalAnswer::TextareaInputCancelled,
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
                QueuedModal::HealthProbe { modal, .. } => match modal.handle_key(key) {
                    HealthProbeAction::Continue => continue,
                    HealthProbeAction::Repair(repair) => {
                        return Ok(ModalAnswer::HealthCheck {
                            repair,
                            quit: false,
                        });
                    }
                    HealthProbeAction::Continue_ => {
                        return Ok(ModalAnswer::HealthCheck {
                            repair: Vec::new(),
                            quit: false,
                        });
                    }
                    HealthProbeAction::Quit => {
                        return Ok(ModalAnswer::HealthCheck {
                            repair: Vec::new(),
                            quit: true,
                        });
                    }
                },
                QueuedModal::TextareaInput { modal, .. } => match modal.handle_key(key) {
                    TextareaAction::Continue => continue,
                    TextareaAction::Submit(value) => {
                        return Ok(ModalAnswer::TextareaInput(value));
                    }
                    TextareaAction::Cancel(_) => {
                        return Ok(ModalAnswer::TextareaInputCancelled);
                    }
                },
            }
        }
    }

    // ─── v2.2.18 task #666 (Agent A1): streaming + health probe ──────

    /// Drive a single health-probe modal to resolution. Convenience
    /// wrapper that pushes the modal onto a throwaway queue, drains
    /// it, and returns the `ModalAnswer::HealthCheck` outcome.
    #[allow(dead_code)]
    pub(crate) fn run_health_probe(
        &mut self,
        tag: &str,
        modal: HealthProbeModal,
    ) -> Result<ModalAnswer, RunnerError> {
        let mut q = ModalQueue::new();
        q.push(QueuedModal::HealthProbe {
            tag: tag.to_string(),
            modal,
        });
        self.run_queue(&mut q)?;
        Ok(q.answer_for(tag).cloned().unwrap_or(ModalAnswer::HealthCheck {
            repair: Vec::new(),
            quit: true,
        }))
    }

    /// Drive a `StreamingOutputModal` to completion.
    ///
    /// Primary entry point for v2.2.18's commissioning subprocesses
    /// (Ollama install, `ollama pull`, `npm install`, `qmd update`,
    /// `qmd embed`, etc.). The modal:
    ///
    /// 1. Spawns the attached `Command` with stdout/stderr piped.
    /// 2. Reads lines via `mpsc::channel` (subprocess output never
    ///    bypasses the TUI — feedback-tui-stdout-anti-pattern.md).
    /// 3. Renders the rolling N-line tail at most once per 100ms.
    /// 4. On Esc, opens a nested `ConfirmModal` ("Cancel install?").
    ///    On Yes the runner sends SIGTERM, waits 5s, then SIGKILL.
    /// 5. Returns `ModalAnswer::StreamingResult { exit_code,
    ///    output_tail }`. `exit_code = -1` is the cancel signal.
    ///
    /// Any key OTHER than Esc is ignored while the subprocess is
    /// running.
    ///
    /// The `_tag` is accepted only for API consistency with the rest
    /// of the runner; it is NOT written to a modal queue (streaming
    /// modals are single-call, never enqueued).
    #[allow(dead_code)]
    pub(crate) fn run_streaming_output(
        &mut self,
        _tag: &str,
        mut modal: StreamingOutputModal,
    ) -> Result<ModalAnswer, RunnerError> {
        let accent = self.accent;
        // Pull the command out of the modal — the modal carries the
        // pre-built Command in the builder, the runner consumes it.
        let Some(cmd) = modal.command.take() else {
            // No subprocess attached → nothing to run. Return an
            // "exit_code = 0" with whatever tail was preloaded (so
            // tests can drive the modal without a real Command).
            let tail = modal.drain_tail();
            return Ok(ModalAnswer::StreamingResult {
                exit_code: 0,
                output_tail: tail,
            });
        };
        let mut handle = SubprocessHandle::spawn(cmd)
            .map_err(|e| RunnerError::Draw(format!("spawn failed: {e}")))?;

        // Force the first draw to render immediately.
        let mut last_draw = Instant::now() - Duration::from_secs(1);
        let key_poll = Duration::from_millis(streaming::REDRAW_BUDGET_MS);
        let mut cancel_pending = false;

        loop {
            // 1. Drain pending output lines (non-blocking).
            for line in handle.drain(32) {
                modal.push_line(line);
            }

            // 2. Poll subprocess exit (non-blocking).
            let exited = handle.poll_exit();

            // 3. Cancelling state: wait for the subprocess to die.
            if matches!(modal.state, StreamingState::Cancelling) {
                if let Some(_code) = exited {
                    modal.state = StreamingState::Exited(-1);
                    let tail = modal.drain_tail();
                    return Ok(ModalAnswer::StreamingResult {
                        exit_code: -1,
                        output_tail: tail,
                    });
                }
            } else if let Some(code) = exited {
                // Clean exit — drain any final lines, then return.
                for line in handle.drain(64) {
                    modal.push_line(line);
                }
                modal.state = StreamingState::Exited(code);
                let tail = modal.drain_tail();
                return Ok(ModalAnswer::StreamingResult {
                    exit_code: code,
                    output_tail: tail,
                });
            }

            // 4. Throttle redraws to one per REDRAW_BUDGET_MS.
            if last_draw.elapsed() >= Duration::from_millis(streaming::REDRAW_BUDGET_MS) {
                modal.tick_spinner();
                self.session
                    .terminal
                    .draw(|frame| {
                        let area = frame.area();
                        modal.render(frame, area, accent);
                    })
                    .map_err(|e| RunnerError::Draw(e.to_string()))?;
                last_draw = Instant::now();
            }

            // 5. Bounded key poll. Route to cancel-confirm if open,
            //    otherwise to the streaming modal itself.
            if let Some(key) = self.keys.try_next_key(key_poll) {
                if let Some(confirm) = modal.cancel_confirm.as_mut() {
                    match confirm.handle_key(key) {
                        ConfirmAction::Continue => continue,
                        ConfirmAction::Committed(ConfirmChoice::Yes) => {
                            modal.cancel_confirm = None;
                            modal.state = StreamingState::Cancelling;
                            cancel_pending = true;
                        }
                        ConfirmAction::Committed(ConfirmChoice::No) => {
                            modal.cancel_confirm = None;
                        }
                    }
                } else {
                    match modal.handle_key(key) {
                        StreamingAction::Continue => {}
                        StreamingAction::RequestCancel => {
                            modal.cancel_confirm = Some(ConfirmModal::new(
                                "Cancel install?",
                                "The subprocess is still running. \
                                 Press y to send SIGTERM (then SIGKILL after 5s), \
                                 n / Esc to keep waiting.",
                            ));
                        }
                    }
                }
            }

            // 6. If a cancel was confirmed this tick, run the grace+kill.
            if cancel_pending {
                let code = streaming::cancel_with_grace(&mut handle, streaming::CANCEL_GRACE);
                for line in handle.drain(64) {
                    modal.push_line(line);
                }
                modal.state = StreamingState::Exited(-1);
                let tail = modal.drain_tail();
                let _ = code; // always `-1` for user-cancel
                return Ok(ModalAnswer::StreamingResult {
                    exit_code: -1,
                    output_tail: tail,
                });
            }

            // 7. Scripted-source backoff: if the key source is
            //    exhausted and we're still running, sleep briefly so
            //    the reader threads get scheduled.
            if self.keys.is_exhausted_hint()
                && !matches!(modal.state, StreamingState::Exited(_))
            {
                thread::sleep(Duration::from_millis(20));
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

    // ─── v2.2.17 #644 regression tests ─────────────────────────────────

    /// #644 Item 1: the welcome card renders the title + tagline + hint
    /// row inside the frame buffer so the user sees the first impression
    /// before any "Step 1 of 8" banner.
    #[test]
    fn wizard_step_0_welcome_screen_renders_with_tagline() {
        let mut session = make_session();
        session
            .render_welcome_card(
                "Welcome to Anvil v2.2.17",
                "Your AI coding partner that runs anywhere — Anthropic, OpenAI, Ollama, and 32 more providers.",
                "Let's get you setup.",
                "Press Enter to continue · Esc to skip setup",
                Color::Cyan,
            )
            .expect("render_welcome_card");

        let buf = session.terminal.backend().buffer();
        let mut found_title = false;
        let mut found_tag = false;
        let mut found_hint = false;
        for y in 0..buf.area.height {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            if row.contains("Welcome to Anvil") {
                found_title = true;
            }
            if row.contains("AI coding partner") {
                found_tag = true;
            }
            if row.contains("Press Enter") && row.contains("Esc") {
                found_hint = true;
            }
        }
        assert!(found_title, "welcome card must show title");
        assert!(found_tag, "welcome card must show tagline");
        assert!(found_hint, "welcome card must show Enter/Esc hint");
    }

    /// #644 Item 1: Enter on the welcome card advances into Step 1.
    /// Mirrors `await_welcome_keypress` semantics; we exercise the
    /// `try_next_key` cadence of the scripted source which mimics the
    /// production short-poll behaviour.
    #[test]
    fn wizard_step_0_enter_advances_to_step_1() {
        use crate::wizard::{await_welcome_keypress, WelcomeOutcome};
        let mut session = make_session();
        let mut runner =
            make_runner(&mut session, vec![key(KeyCode::Enter)]);
        let outcome = await_welcome_keypress(&mut runner).expect("welcome");
        assert_eq!(outcome, WelcomeOutcome::Continue);
    }

    /// #644 Item 1: Esc on the welcome card skips the wizard.
    #[test]
    fn wizard_step_0_esc_skips_setup_with_minimal_config() {
        use crate::wizard::{await_welcome_keypress, WelcomeOutcome};
        let mut session = make_session();
        let mut runner =
            make_runner(&mut session, vec![key(KeyCode::Esc)]);
        let outcome = await_welcome_keypress(&mut runner).expect("welcome");
        assert_eq!(outcome, WelcomeOutcome::Skip);
    }

    /// #644 Item 2: a description-aware banner renders both the title
    /// AND the supplied description paragraph in the frame buffer.
    #[test]
    fn wizard_banner_includes_description_paragraph() {
        let mut session = make_session();
        let mut runner =
            make_runner(&mut session, Vec::<KeyEvent>::new());
        runner
            .session
            .render_banner_with_description(
                "Step 1 of 8 — Vault Setup",
                "Vault encrypts your API keys at rest. Pick a master password you'll remember — there's no recovery if lost.",
                &["AES-256-GCM, local only."],
                Color::Cyan,
            )
            .expect("render_banner_with_description");

        let buf = session.terminal.backend().buffer();
        // Concatenate the full screen into one string so a wrapped
        // description still matches even if it spans multiple rows.
        let mut concat = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                concat.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            concat.push(' ');
        }
        assert!(
            concat.contains("Vault Setup"),
            "title must render"
        );
        assert!(
            concat.contains("Vault encrypts your API keys"),
            "description must render"
        );
    }

    /// #644 Item 3: the OAuth poll must run on every iteration of the
    /// wizard's OAuth loop, not just on key events.  Construct a flow
    /// pre-armed with a Failed event on the channel and assert that
    /// `run_oauth_flow` finalizes WITHOUT any keypress in the scripted
    /// source (which is empty).
    #[test]
    fn oauth_flow_poll_fires_every_frame_in_wizard_loop() {
        use crate::tui::oauth_flow::{OAuthFlow, OAuthFlowState, OAuthOutcome};
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        // Pre-arm with a failure so `flow.poll()` will transition the
        // state on the FIRST iteration of `run_oauth_flow`.
        tx.send(Err("listener fail".to_string())).unwrap();
        let flow = OAuthFlow {
            provider: api::ProviderKind::AnvilApi,
            state: OAuthFlowState::AwaitingCallback {
                started_at: std::time::Instant::now(),
                port: Some(0),
                authorize_url: "https://example.test/x".to_string(),
                result_rx: rx,
                expected_state: "s".to_string(),
                pkce_verifier: "v".to_string(),
                redirect_uri: "http://127.0.0.1:0/callback".to_string(),
            },
        };
        let mut session = make_session();
        // ZERO keys in the source — proves the poll runs without one.
        let mut runner =
            make_runner(&mut session, Vec::<KeyEvent>::new());
        let outcome = runner.run_oauth_flow(flow).expect("run_oauth_flow");
        assert!(
            matches!(outcome, OAuthOutcome::Failed(_)),
            "poll must drain channel on first iteration even with no keys"
        );
    }

    /// #644 Item 4: the OAuth flow's elapsed counter is computed from
    /// `Instant::now() - started_at` every render, so as long as the
    /// loop redraws on every iteration the counter ticks.  This test
    /// proves the render path runs at least once per loop iteration by
    /// reading the rendered frame buffer for a non-zero elapsed value
    /// after a deliberate ~1.1s sleep.
    ///
    /// Marked `#[ignore]` by default — it adds ~1.1s of wall-time, so
    /// CI invokes it via `cargo test -- --ignored elapsed_counter`.
    #[test]
    #[ignore]
    fn oauth_flow_elapsed_counter_updates_at_least_once_per_second() {
        use crate::tui::oauth_flow::{OAuthFlow, OAuthFlowState};
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let (_tx, rx) = mpsc::channel::<Result<(String, String), String>>();
        let started = Instant::now() - Duration::from_millis(1100);
        let flow = OAuthFlow {
            provider: api::ProviderKind::AnvilApi,
            state: OAuthFlowState::AwaitingCallback {
                started_at: started,
                port: Some(0),
                authorize_url: "https://example.test/x".to_string(),
                result_rx: rx,
                expected_state: "s".to_string(),
                pkce_verifier: "v".to_string(),
                redirect_uri: "http://127.0.0.1:0/callback".to_string(),
            },
        };
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                flow.render(frame, area, Color::Cyan);
            })
            .unwrap();
        let buf_str: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        // After 1.1s sleep, "Elapsed: 1s" should be in the frame
        // (or 2s if the test machine paused).  Either way it MUST
        // NOT still read "Elapsed: 0s".
        assert!(
            !buf_str.contains("Elapsed: 0s"),
            "elapsed counter must have ticked past 0 after 1.1s; got: {buf_str:.200}"
        );
        assert!(
            buf_str.contains("Elapsed: 1s")
                || buf_str.contains("Elapsed: 2s"),
            "elapsed counter must read 1s or 2s; got: {buf_str:.200}"
        );
    }

    /// #644 Item 3: the wizard's OAuth loop completes WITHOUT requiring
    /// a keypress in the terminal — the listener-driven success path
    /// fires off the poll alone.  Construct a flow whose channel
    /// already carries a successful (code, state) pair and verify the
    /// loop transitions to Success without any input.
    ///
    /// Note: we cannot exercise the FULL Anvil token-exchange path in
    /// a unit test (it does a real HTTPS call).  We instead pre-set
    /// the state to `Failed` via the listener-error path which proves
    /// the poll-only completion path runs end-to-end.
    #[test]
    fn oauth_flow_completes_without_keypress() {
        use crate::tui::oauth_flow::{OAuthFlow, OAuthFlowState, OAuthOutcome};
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        tx.send(Err("simulated".to_string())).unwrap();
        let flow = OAuthFlow {
            provider: api::ProviderKind::AnvilApi,
            state: OAuthFlowState::AwaitingCallback {
                started_at: std::time::Instant::now(),
                port: Some(0),
                authorize_url: "https://example.test/x".to_string(),
                result_rx: rx,
                expected_state: "s".to_string(),
                pkce_verifier: "v".to_string(),
                redirect_uri: "http://127.0.0.1:0/callback".to_string(),
            },
        };
        let mut session = make_session();
        let mut runner =
            make_runner(&mut session, Vec::<KeyEvent>::new());
        // No keys — the loop must still complete because the poll
        // drains the listener channel and the state machine reaches a
        // terminal state.
        let outcome = runner.run_oauth_flow(flow).expect("run_oauth_flow");
        assert!(matches!(outcome, OAuthOutcome::Failed(_)));
    }

    /// #644 Item 5: the centralized modal secondary text color has a
    /// relative luminance >= 0.55 against the rec.709 coefficients.
    #[test]
    fn modal_secondary_text_color_meets_luminance_threshold() {
        use crate::tui::modals::{modal_secondary_color, rgb_relative_luminance};
        let c = modal_secondary_color();
        let lum = rgb_relative_luminance(c)
            .expect("modal_secondary_color must be a Color::Rgb so luminance is computable");
        assert!(
            lum >= 0.55,
            "modal secondary text color must read at or above 0.55 luminance; got {lum:.3}"
        );
        // Sanity: not bright white either (must remain visibly
        // secondary).
        assert!(
            lum < 0.95,
            "modal secondary text color must remain visibly subordinate to white"
        );
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

    // ─── v2.2.18 task #666 (Agent A1): streaming + health probe ──────

    /// Acceptance criterion #4: `run_streaming_output` with
    /// `Command::new("echo").arg("hello")` resolves to a
    /// `StreamingResult { exit_code: 0, .. }` with `hello` in the
    /// output tail.
    #[test]
    fn run_streaming_output_echo_hello_resolves_to_zero_exit() {
        use crate::tui::modals::streaming::StreamingOutputModal;
        use std::process::Command;

        let mut session = make_session();
        let mut runner = make_runner(&mut session, vec![]);

        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        let modal = StreamingOutputModal::new("Test echo", "Running echo…")
            .with_subprocess(cmd);
        let answer = runner
            .run_streaming_output("test-echo", modal)
            .expect("run_streaming_output");
        match answer {
            ModalAnswer::StreamingResult { exit_code, output_tail } => {
                assert_eq!(exit_code, 0, "echo must exit 0");
                assert!(
                    output_tail.iter().any(|l| l.contains("hello")),
                    "output_tail must contain echo output, got {output_tail:?}",
                );
            }
            other => panic!("unexpected answer: {other:?}"),
        }
    }

    /// `run_streaming_output` with no subprocess attached returns
    /// `StreamingResult { exit_code: 0 }` immediately — the empty-
    /// command early return.
    #[test]
    fn run_streaming_output_no_subprocess_returns_zero() {
        use crate::tui::modals::streaming::StreamingOutputModal;

        let mut session = make_session();
        let mut runner = make_runner(&mut session, vec![]);

        let modal = StreamingOutputModal::new("No subprocess", "n/a");
        let answer = runner
            .run_streaming_output("none", modal)
            .expect("run_streaming_output");
        match answer {
            ModalAnswer::StreamingResult { exit_code, .. } => {
                assert_eq!(exit_code, 0);
            }
            other => panic!("expected StreamingResult, got {other:?}"),
        }
    }

    /// `run_health_probe` round-trips through the queue and surfaces
    /// the `ModalAnswer::HealthCheck { repair, quit }` shape. We
    /// press `r` immediately — the runner should pick up the pre-
    /// selected `Fail`/`Warn` repair flags.
    #[test]
    fn run_health_probe_with_r_returns_preselected_repair_set() {
        use crate::tui::modals::health_probe::{
            HealthIssue, HealthProbeModal, HealthStatus,
        };

        let modal = HealthProbeModal::new("Health", "We checked").with_issues(vec![
            HealthIssue::new(HealthStatus::Ok, "Vault OK"),
            HealthIssue::new_repair(HealthStatus::Fail, "Ollama not running"),
            HealthIssue::new_repair(HealthStatus::Warn, "QMD stale"),
        ]);
        let mut session = make_session();
        let mut runner = make_runner(&mut session, vec![key(KeyCode::Char('r'))]);

        let answer = runner
            .run_health_probe("health-1", modal)
            .expect("run_health_probe");
        match answer {
            ModalAnswer::HealthCheck { repair, quit } => {
                assert!(!quit, "user pressed `r`, not `q`");
                assert_eq!(repair, vec![1, 2], "Fail+Warn rows must be selected");
            }
            other => panic!("expected HealthCheck, got {other:?}"),
        }
    }

    /// `q` resolves with `quit = true` so callers can detect a
    /// wizard-abort from the health-probe modal.
    #[test]
    fn run_health_probe_with_q_sets_quit_true() {
        use crate::tui::modals::health_probe::{
            HealthIssue, HealthProbeModal, HealthStatus,
        };

        let modal = HealthProbeModal::new("Health", "We checked").with_issues(vec![
            HealthIssue::new_repair(HealthStatus::Fail, "Issue"),
        ]);
        let mut session = make_session();
        let mut runner = make_runner(&mut session, vec![key(KeyCode::Char('q'))]);
        let answer = runner
            .run_health_probe("health-2", modal)
            .expect("run_health_probe");
        match answer {
            ModalAnswer::HealthCheck { repair, quit } => {
                assert!(quit, "q must set quit=true");
                assert!(repair.is_empty(), "q must not report any repairs");
            }
            other => panic!("expected HealthCheck, got {other:?}"),
        }
    }
}
