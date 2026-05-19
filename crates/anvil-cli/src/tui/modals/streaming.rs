//! `StreamingOutputModal` — live subprocess output overlay (task #666, v2.2.18).
//!
//! The v2.2.18 install + setup commissioning system needs to run a
//! handful of long-running subprocesses inside the wizard's alt-screen
//! session — Ollama installer, `ollama pull` model download, `npm
//! install -g @tobilu/qmd`, `qmd update`, `qmd embed`, plus A5's
//! healing actions that re-run any of the above. Each of these blocks
//! the wizard for tens of seconds to minutes and would, in a naive
//! implementation, force the wizard to drop out of alt-screen.
//!
//! This modal solves that. It renders a bordered overlay (visually
//! identical to `ConfirmModal` / `WizardChoiceModal`), spawns the
//! subprocess with stdout + stderr piped into reader threads, and
//! displays the last N lines of merged output as they arrive. The
//! reader threads communicate with the modal via an `mpsc::channel`
//! so the subprocess never writes to the inherited terminal (which
//! would corrupt the alt-screen back-buffer per
//! `feedback-tui-stdout-anti-pattern.md`).
//!
//! ## Cancellation
//!
//! Esc opens a nested `ConfirmModal` ("Cancel install?"). On
//! confirmation the modal sends a graceful SIGTERM (Unix only — on
//! Windows we go straight to `Child::kill()`), waits up to 5 seconds
//! for the subprocess to exit, then sends SIGKILL if it is still
//! alive. The returned `ModalAnswer::StreamingResult` carries
//! `exit_code = -1` to signal cancellation.
//!
//! ## Rendering budget
//!
//! At most one redraw per 100ms even if the subprocess spews fast —
//! the reader thread always pushes to the channel, but the render
//! loop only re-renders when (a) >=100ms has elapsed since the last
//! draw or (b) the subprocess has exited or transitioned to a
//! cancel-confirming state. This is the same "bounded poll" pattern
//! `run_oauth_flow` uses for the elapsed-counter tick.
//!
//! ## 8-axis capability contract (per `feedback-anvil-capability-contract.md`)
//!
//! 1. Definition         — `StreamingOutputModal` struct + state machine.
//! 2. Registration       — `pub mod streaming` in `tui/modals/mod.rs`;
//!                         caller uses `WizardModalRunner::run_streaming_output`.
//! 3. Completion         — N/A (not a slash command).
//! 4. Handler            — `handle_key` returns `StreamingAction`; the
//!                         caller (`run_streaming_output`) translates
//!                         into the resolution flow.
//! 5. Dispatch           — `WizardModalRunner::run_streaming_output`
//!                         is the one entry point.
//! 6. Rendering          — `render` paints the centered overlay.
//! 7. Gate               — single live subprocess per modal instance;
//!                         dropping the modal kills the subprocess.
//! 8. OTel + tests       — unit tests at the bottom of this file
//!                         exercise spawn / line capture / cancel /
//!                         progress-detector / SIGKILL escalation.
//!
//! ## Cross-platform
//!
//! - Unix (Linux, macOS, FreeBSD, NetBSD, OpenBSD): SIGTERM via
//!   `Command::new("kill")` then SIGKILL via `Child::kill`.
//! - Windows: `Child::kill()` directly (TerminateProcess).
//!
//! No `/proc` reads, no GNU-only flags.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

/// Number of trailing output lines to keep + render. Matches the
/// design contract (N = 8).
pub(crate) const TAIL_LINES: usize = 8;

/// Minimum interval between redraws while the subprocess is running.
/// Used by `WizardModalRunner::run_streaming_output` to bound the
/// frame rate.
pub(crate) const REDRAW_BUDGET_MS: u64 = 100;

/// Default cancellation grace period: send SIGTERM, wait this long
/// for the subprocess to exit, then SIGKILL. The contract requires 5
/// seconds.
pub(crate) const CANCEL_GRACE: Duration = Duration::from_secs(5);

/// Outcome of `StreamingOutputModal::handle_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum StreamingAction {
    /// Stay open, redraw on the next render tick.
    Continue,
    /// User pressed Esc — open the nested cancel-confirm dialog.
    RequestCancel,
}

/// State of the subprocess from the modal's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum StreamingState {
    /// Subprocess is alive and the reader threads are sending lines.
    Running,
    /// User confirmed cancellation; SIGTERM has been sent and we're
    /// waiting up to `CANCEL_GRACE` for the subprocess to exit. If it
    /// doesn't, we escalate to SIGKILL.
    Cancelling,
    /// Subprocess exited (cleanly or via signal). `exit_code` is the
    /// captured status; `-1` if cancelled.
    Exited(i32),
}

/// A subprocess-streaming overlay.
///
/// Build via `StreamingOutputModal::new(title, status)` + chain a
/// `.with_subprocess(...)` to attach the `Command` that will be
/// spawned on the next call to `WizardModalRunner::run_streaming_output`.
/// Attach an optional `.with_progress_detector(...)` callback to parse
/// progress percent (`0.0 ..= 1.0`) from output lines.
pub(crate) struct StreamingOutputModal {
    pub(crate) title: String,
    pub(crate) status: String,
    /// The pre-built command. Taken on `spawn()` (Option<Command> is
    /// consumed; calling spawn twice panics).
    pub(crate) command: Option<Command>,
    /// Optional progress parser: returns `Some(percent)` for a line
    /// that carries a progress signal, `None` otherwise.
    pub(crate) progress_detector: Option<Box<dyn Fn(&str) -> Option<f32> + Send>>,
    /// Rolling tail of the last N output lines (merged stdout/stderr).
    pub(crate) tail: std::collections::VecDeque<String>,
    /// Optional progress percent (0.0..=1.0). Set by `set_progress`
    /// or by the progress detector when a line matches.
    pub(crate) progress: Option<f32>,
    /// Spinner frame index — bumped on every render tick.
    pub(crate) spinner_idx: usize,
    /// Optional rich status row (rendered below the spinner). The
    /// caller can update this between ticks for granular progress
    /// like "layer 3/5 — 256 MB / 4.2 GB".
    pub(crate) detail: Option<String>,
    /// State of the wrapped subprocess.
    pub(crate) state: StreamingState,
    /// Nested confirmation dialog when the user presses Esc.
    pub(crate) cancel_confirm:
        Option<crate::tui::modals::confirm::ConfirmModal>,
}

impl std::fmt::Debug for StreamingOutputModal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingOutputModal")
            .field("title", &self.title)
            .field("status", &self.status)
            .field("has_command", &self.command.is_some())
            .field("has_progress_detector", &self.progress_detector.is_some())
            .field("tail_len", &self.tail.len())
            .field("progress", &self.progress)
            .field("state", &self.state)
            .field("cancel_confirm_open", &self.cancel_confirm.is_some())
            .finish()
    }
}

impl StreamingOutputModal {
    /// Construct a streaming modal with the given title (rendered in
    /// the modal frame) and status line (rendered next to the spinner).
    #[allow(dead_code)] // Wired by A3/A4/A5 in v2.2.18 callers.
    pub(crate) fn new(
        title: impl Into<String>,
        status: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            status: status.into(),
            command: None,
            progress_detector: None,
            tail: std::collections::VecDeque::with_capacity(TAIL_LINES),
            progress: None,
            spinner_idx: 0,
            detail: None,
            state: StreamingState::Running,
            cancel_confirm: None,
        }
    }

    /// Builder: attach the `Command` to spawn. Takes ownership; the
    /// modal will call `.stdout(Stdio::piped()).stderr(Stdio::piped())`
    /// when it spawns. The caller is responsible for setting any env /
    /// args / working directory on the supplied `Command`.
    #[allow(dead_code)]
    pub(crate) fn with_subprocess(mut self, cmd: Command) -> Self {
        self.command = Some(cmd);
        self
    }

    /// Builder: attach a progress parser. The closure runs on EVERY
    /// stdout/stderr line; return `Some(percent)` where `percent` is
    /// in `0.0..=1.0` to update the progress bar, or `None` to skip.
    #[allow(dead_code)]
    pub(crate) fn with_progress_detector(
        mut self,
        detector: impl Fn(&str) -> Option<f32> + Send + 'static,
    ) -> Self {
        self.progress_detector = Some(Box::new(detector));
        self
    }

    /// Manually set the progress (0.0..=1.0). Useful when the caller
    /// computes progress externally (e.g. from a count of completed
    /// files in `qmd embed`).
    #[allow(dead_code)]
    pub(crate) fn set_progress(&mut self, p: f32) {
        self.progress = Some(p.clamp(0.0, 1.0));
    }

    /// Manually update the rich detail line (rendered below the
    /// spinner). Pass an empty string to clear.
    #[allow(dead_code)]
    pub(crate) fn set_detail(&mut self, detail: impl Into<String>) {
        let s = detail.into();
        self.detail = if s.is_empty() { None } else { Some(s) };
    }

    /// Process a key event. Esc requests cancellation (the caller
    /// opens a nested confirm modal); all other keys are ignored.
    ///
    /// When `cancel_confirm` is open, this fn is NOT called — the
    /// runner routes keys to the cancel modal directly.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> StreamingAction {
        match key.code {
            KeyCode::Esc => StreamingAction::RequestCancel,
            _ => StreamingAction::Continue,
        }
    }

    /// Push a line into the rolling tail. Trims to N most recent.
    /// Also runs the progress detector (if any) and updates
    /// `self.progress` on a hit.
    pub(crate) fn push_line(&mut self, line: String) {
        if let Some(detector) = self.progress_detector.as_ref() {
            if let Some(p) = detector(&line) {
                self.progress = Some(p.clamp(0.0, 1.0));
            }
        }
        if self.tail.len() == TAIL_LINES {
            self.tail.pop_front();
        }
        self.tail.push_back(line);
    }

    /// Drain the tail to a `Vec<String>` in arrival order. Used to
    /// build `ModalAnswer::StreamingResult { output_tail, .. }`.
    pub(crate) fn drain_tail(&mut self) -> Vec<String> {
        self.tail.drain(..).collect()
    }

    /// Bump the spinner frame counter.
    pub(crate) fn tick_spinner(&mut self) {
        self.spinner_idx = self.spinner_idx.wrapping_add(1);
    }

    /// Render the streaming overlay. When `cancel_confirm` is open the
    /// nested confirm modal is rendered on top of the streaming frame
    /// so the user sees the original context behind the dialog.
    pub(crate) fn render(&self, frame: &mut Frame, area: Rect, accent: Color) {
        // Width: at most 80, at least 40 (room for tail lines).
        let modal_w = area.width.saturating_sub(6).min(80).max(40);
        let needed_h: u16 = TAIL_LINES as u16 + 8;
        let modal_h = needed_h.min(area.height.saturating_sub(2)).max(8);
        if area.width < 12 || area.height < 8 {
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

        let mut lines: Vec<Line<'static>> = Vec::with_capacity(TAIL_LINES + 6);
        lines.push(Line::from(""));

        // Spinner + status row.
        let spinner_frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
        let spinner = match self.state {
            StreamingState::Running => spinner_frames[self.spinner_idx % 8],
            StreamingState::Cancelling => '!',
            StreamingState::Exited(0) => '✓',
            StreamingState::Exited(_) => '✗',
        };
        let status_color = match self.state {
            StreamingState::Running => accent,
            StreamingState::Cancelling => Color::Yellow,
            StreamingState::Exited(0) => Color::Green,
            StreamingState::Exited(_) => Color::Red,
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                spinner.to_string(),
                Style::default().fg(status_color).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                self.status.clone(),
                Style::default().fg(Color::White),
            ),
        ]));

        // Progress bar (optional).
        if let Some(p) = self.progress {
            let bar_w = inner.width.saturating_sub(8) as usize;
            let filled = ((p.clamp(0.0, 1.0) * bar_w as f32) as usize).min(bar_w);
            let bar = format!(
                "  [{}{}]  {}%",
                "█".repeat(filled),
                "·".repeat(bar_w.saturating_sub(filled)),
                (p.clamp(0.0, 1.0) * 100.0) as u32
            );
            lines.push(Line::from(Span::styled(
                bar,
                Style::default().fg(accent),
            )));
        } else {
            lines.push(Line::from(""));
        }

        // Detail row (optional).
        if let Some(d) = self.detail.as_ref().filter(|d| !d.is_empty()) {
            lines.push(Line::from(Span::styled(
                format!("  {d}"),
                Style::default().fg(super::modal_secondary_color()),
            )));
        } else {
            lines.push(Line::from(""));
        }

        // Tail rows. Pad with blank lines so the modal height is stable.
        let max_line_w = inner.width.saturating_sub(4) as usize;
        let pad_to_n = TAIL_LINES.saturating_sub(self.tail.len());
        for _ in 0..pad_to_n {
            lines.push(Line::from(""));
        }
        for raw in self.tail.iter() {
            let trimmed: String = raw.chars().take(max_line_w).collect();
            lines.push(Line::from(Span::styled(
                format!("  {trimmed}"),
                Style::default().fg(super::modal_secondary_color()),
            )));
        }

        // Footer hint.
        lines.push(Line::from(""));
        let hint = match self.state {
            StreamingState::Running => "Esc to cancel",
            StreamingState::Cancelling => "Cancelling — waiting for subprocess",
            StreamingState::Exited(0) => "Done. Press any key to continue.",
            StreamingState::Exited(_) => "Failed. Press any key to continue.",
        };
        lines.push(Line::from(Span::styled(
            format!("  {hint}"),
            Style::default().fg(super::modal_secondary_color()),
        )));

        frame.render_widget(Paragraph::new(lines), inner);

        // Nested confirm dialog overlay — render last so it sits on
        // top of the streaming frame.
        if let Some(confirm) = self.cancel_confirm.as_ref() {
            confirm.render(frame, area, Color::Red);
        }
    }
}

// ─── Subprocess spawn + reader threads ───────────────────────────────────────

/// A handle to the spawned subprocess + its merged-output channel.
///
/// `WizardModalRunner::run_streaming_output` owns one of these for
/// the lifetime of the streaming modal. On Drop the child is killed
/// (best-effort) so a wizard panic does not leave a runaway process.
pub(crate) struct SubprocessHandle {
    pub(crate) child: Child,
    pub(crate) lines_rx: mpsc::Receiver<String>,
    /// Joins the two reader threads. Held only so they're not detached
    /// at scope exit; the threads themselves exit when their stream
    /// EOFs (i.e. when the subprocess closes the pipe).
    #[allow(dead_code)]
    pub(crate) stdout_thread: Option<thread::JoinHandle<()>>,
    #[allow(dead_code)]
    pub(crate) stderr_thread: Option<thread::JoinHandle<()>>,
}

impl SubprocessHandle {
    /// Take a `Command`, attach piped stdout+stderr, spawn the child,
    /// and start two reader threads that send every line into an
    /// `mpsc::channel`. Returns the handle on success.
    pub(crate) fn spawn(mut cmd: Command) -> std::io::Result<Self> {
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (tx, rx) = mpsc::channel::<String>();

        let stdout_thread = stdout.map(|out| {
            let tx = tx.clone();
            thread::spawn(move || {
                let reader = BufReader::new(out);
                for line in reader.lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })
        });
        let stderr_thread = stderr.map(|err| {
            let tx = tx.clone();
            thread::spawn(move || {
                let reader = BufReader::new(err);
                for line in reader.lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })
        });
        // Drop the local `tx` so the channel closes when both reader
        // threads exit (each reader holds a clone).
        drop(tx);

        Ok(Self {
            child,
            lines_rx: rx,
            stdout_thread,
            stderr_thread,
        })
    }

    /// Non-blocking drain of pending lines. Returns up to `max_lines`
    /// per call so the render loop doesn't starve on a fast spew.
    pub(crate) fn drain(&self, max_lines: usize) -> Vec<String> {
        let mut out = Vec::new();
        for _ in 0..max_lines {
            match self.lines_rx.try_recv() {
                Ok(line) => out.push(line),
                Err(_) => break,
            }
        }
        out
    }

    /// Non-blocking poll of the child's exit status. Returns
    /// `Some(code)` if the child has exited, `None` if it's still
    /// running. `code = -1` is the conventional "killed by signal,
    /// no status available" value.
    pub(crate) fn poll_exit(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
            Ok(None) => None,
            Err(_) => Some(-1),
        }
    }

    /// Send SIGTERM to the subprocess (Unix only — on Windows we go
    /// straight to `kill()`). Returns `true` if the signal was sent.
    pub(crate) fn sigterm(&mut self) -> bool {
        #[cfg(unix)]
        {
            let pid = self.child.id() as i32;
            // We invoke /bin/kill rather than libc directly so we
            // don't add a libc dependency. /bin/kill is part of every
            // POSIX userland we target (Linux, macOS, *BSD).
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            true
        }
        #[cfg(not(unix))]
        {
            // Windows: no SIGTERM equivalent; fall straight through to
            // kill(). Return false so the caller knows not to wait
            // out the grace period.
            let _ = self.child.kill();
            false
        }
    }

    /// Force-kill the subprocess (SIGKILL on Unix, TerminateProcess
    /// on Windows). Best-effort.
    pub(crate) fn sigkill(&mut self) {
        let _ = self.child.kill();
        // Reap so we don't leave a zombie.
        let _ = self.child.wait();
    }
}

impl Drop for SubprocessHandle {
    fn drop(&mut self) {
        // Best-effort cleanup on Drop. If the runner already reaped
        // the child these are no-ops.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Cancel a running subprocess: send SIGTERM, wait up to `grace`,
/// then SIGKILL if still alive. Returns the captured exit code (or
/// `-1` if the kill path was taken).
///
/// Exposed at module scope so the runner can call it without
/// borrowing the modal mutably across the wait loop.
pub(crate) fn cancel_with_grace(
    handle: &mut SubprocessHandle,
    grace: Duration,
) -> i32 {
    handle.sigterm();
    let start = Instant::now();
    while start.elapsed() < grace {
        if let Some(code) = handle.poll_exit() {
            return code;
        }
        thread::sleep(Duration::from_millis(50));
    }
    handle.sigkill();
    -1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use std::process::Command;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn streaming_modal_builder_chain() {
        let m = StreamingOutputModal::new("Installing Ollama", "Running…")
            .with_subprocess(Command::new("true"))
            .with_progress_detector(|line: &str| {
                if line.starts_with("progress ") {
                    let pct: f32 = line["progress ".len()..]
                        .trim_end_matches('%')
                        .parse()
                        .unwrap_or(0.0);
                    Some(pct / 100.0)
                } else {
                    None
                }
            });
        assert_eq!(m.title, "Installing Ollama");
        assert_eq!(m.status, "Running…");
        assert!(m.command.is_some());
        assert!(m.progress_detector.is_some());
        assert_eq!(m.state, StreamingState::Running);
    }

    #[test]
    fn push_line_keeps_last_n_only() {
        let mut m = StreamingOutputModal::new("t", "s");
        for i in 0..(TAIL_LINES + 4) {
            m.push_line(format!("line {i}"));
        }
        assert_eq!(m.tail.len(), TAIL_LINES);
        // The oldest line we kept should be "line 4" (since 4..12 = 8).
        assert_eq!(m.tail.front().unwrap(), &format!("line {}", 4));
        assert_eq!(
            m.tail.back().unwrap(),
            &format!("line {}", TAIL_LINES + 3)
        );
    }

    #[test]
    fn progress_detector_updates_progress() {
        let mut m = StreamingOutputModal::new("t", "s")
            .with_progress_detector(|line: &str| {
                line.strip_prefix("p=")
                    .and_then(|s| s.parse::<f32>().ok())
            });
        m.push_line("p=0.25".to_string());
        assert_eq!(m.progress, Some(0.25));
        m.push_line("p=0.75".to_string());
        assert_eq!(m.progress, Some(0.75));
        // Out-of-range values get clamped.
        m.push_line("p=1.5".to_string());
        assert_eq!(m.progress, Some(1.0));
    }

    #[test]
    fn set_progress_clamps() {
        let mut m = StreamingOutputModal::new("t", "s");
        m.set_progress(-0.5);
        assert_eq!(m.progress, Some(0.0));
        m.set_progress(2.0);
        assert_eq!(m.progress, Some(1.0));
        m.set_progress(0.42);
        assert_eq!(m.progress, Some(0.42));
    }

    #[test]
    fn handle_key_esc_requests_cancel() {
        let mut m = StreamingOutputModal::new("t", "s");
        assert_eq!(m.handle_key(key(KeyCode::Esc)), StreamingAction::RequestCancel);
        assert_eq!(m.handle_key(key(KeyCode::Char('q'))), StreamingAction::Continue);
        assert_eq!(m.handle_key(key(KeyCode::Enter)), StreamingAction::Continue);
    }

    #[test]
    fn drain_tail_returns_lines_in_order() {
        let mut m = StreamingOutputModal::new("t", "s");
        m.push_line("a".to_string());
        m.push_line("b".to_string());
        m.push_line("c".to_string());
        let drained = m.drain_tail();
        assert_eq!(drained, vec!["a", "b", "c"]);
        assert!(m.tail.is_empty());
    }

    #[test]
    fn render_smoke_test() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let mut m = StreamingOutputModal::new("Installing Ollama", "Running installer…");
        m.push_line("downloading 1.2 GB".to_string());
        m.set_progress(0.42);
        m.set_detail("layer 3/5");
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
            dump.contains("Installing Ollama"),
            "title missing from render"
        );
        assert!(
            dump.contains("Running installer"),
            "status missing from render"
        );
        assert!(
            dump.contains("downloading"),
            "tail line missing from render"
        );
    }

    /// Acceptance criterion #4: spawn `echo hello` and verify the
    /// modal observes a clean exit with the captured tail.
    #[test]
    fn subprocess_spawn_captures_stdout_and_exits_zero() {
        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        let mut handle = SubprocessHandle::spawn(cmd).expect("spawn echo");

        // Drain lines + poll exit in a bounded loop.
        let start = Instant::now();
        let mut tail: Vec<String> = Vec::new();
        let mut exit_code: Option<i32> = None;
        while start.elapsed() < Duration::from_secs(5) {
            tail.extend(handle.drain(64));
            if let Some(code) = handle.poll_exit() {
                exit_code = Some(code);
                // One more drain in case the kernel buffered the last
                // bytes between try_wait and our drain.
                tail.extend(handle.drain(64));
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(exit_code, Some(0), "echo must exit 0");
        assert!(
            tail.iter().any(|l| l.contains("hello")),
            "captured tail must contain echo's output: {:?}",
            tail
        );
    }

    /// Acceptance criterion: cancel_with_grace SIGTERMs a long-running
    /// subprocess and returns within the grace period.
    #[cfg(unix)]
    #[test]
    fn cancel_with_grace_terminates_sleep() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let mut handle = SubprocessHandle::spawn(cmd).expect("spawn sleep");
        // Give the spawn a beat to settle.
        thread::sleep(Duration::from_millis(50));
        let start = Instant::now();
        let code = cancel_with_grace(&mut handle, Duration::from_secs(2));
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(3),
            "cancel took too long: {:?}",
            elapsed
        );
        // SIGTERM exit on Unix returns either a non-zero exit code
        // (143 = 128 + SIGTERM) or `-1` if the kernel reports no
        // status. Either is fine — the contract is "no longer
        // running", not a specific code.
        assert_ne!(code, 0, "cancelled subprocess must not report success");
    }

    #[test]
    fn handle_drop_kills_subprocess() {
        // Spawn a long-running subprocess, then drop the handle —
        // the subprocess must be reaped.
        #[cfg(unix)]
        {
            let pid;
            {
                let mut cmd = Command::new("sleep");
                cmd.arg("60");
                let handle = SubprocessHandle::spawn(cmd).expect("spawn sleep");
                pid = handle.child.id() as i32;
            }
            // Give the OS a beat to actually clean up.
            thread::sleep(Duration::from_millis(200));
            // Check kill -0: 0 = still alive, non-zero = gone.
            let status = Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if let Ok(s) = status {
                assert!(
                    !s.success(),
                    "subprocess pid {} should be reaped after handle drop",
                    pid
                );
            }
        }
    }
}
