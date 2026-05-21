//! Task #716 (CC v2.1.145 parity): spinner / elapsed-time wake pump.
//!
//! CC v2.1.145 fixed a freeze where the streaming spinner and the
//! "thinking for Ns" elapsed-time line stopped animating after a terminal
//! refocus (`FocusGained`/`FocusLost`) or resize event, until the user
//! pressed a key. Anvil shipped the same shape of bug: the TUI input
//! loops bracketed their spinner advance with a `crossterm::event::poll`,
//! and `Resize`/`FocusGained`/`FocusLost` events arrived on the same
//! queue but were swallowed by the catch-all match arm without scheduling
//! a redraw. Worse, the only TUI-visible signal that "time is passing"
//! came from the natural cadence of `read_input` iterations — so during
//! a quiet window between key events, ratatui's frame-diff coalesced and
//! the visible spinner glyph never updated.
//!
//! This module provides two small, pure helpers:
//!   - [`SpinnerTimer`]: tracks the last frame-advance instant and
//!     reports whether enough wall-clock time has elapsed to bump the
//!     spinner frame. It is wall-clock-driven, NOT input-driven.
//!   - [`classify_terminal_event`]: maps `crossterm::event::Event` shapes
//!     that are PURE structural notifications (Resize, FocusGained,
//!     FocusLost) onto a small enum so callers can route them to a
//!     redraw scheduler without depending on the crossterm event
//!     constructors at the call site.
//!
//! Both helpers are unit-testable without a real terminal. The
//! regression tests live below and pin the contract: ticks advance the
//! spinner; a simulated `Resize` event in the middle of a tick sequence
//! does NOT freeze further ticks.

use std::time::{Duration, Instant};

use crossterm::event::Event as CtEvent;

/// Default cadence at which the TUI advances the spinner. Matches the
/// existing `SPINNER_TICK_INTERVAL` constants in `tui::mod` so the
/// timer's pump-rate stays consistent across the wait loops.
pub const DEFAULT_SPINNER_INTERVAL: Duration = Duration::from_millis(80);

/// Wall-clock spinner pump.
///
/// Track the last frame-advance instant. Each call to
/// [`SpinnerTimer::tick_at`] reports whether enough time has elapsed
/// since the previous advance to bump the spinner frame. The timer is
/// independent of any crossterm event arrival — a sequence of `tick_at`
/// calls separated by ≥ `interval` will each report `true`, no matter
/// what events did or didn't arrive in the meantime.
///
/// The "test" entry points take an `Instant` explicitly so tests can
/// drive a deterministic clock; production code uses [`SpinnerTimer::tick`]
/// which queries `Instant::now()`.
#[derive(Debug, Clone)]
pub struct SpinnerTimer {
    interval: Duration,
    last_advance: Instant,
}

impl SpinnerTimer {
    /// Construct a new timer with the default 80ms cadence, anchored to
    /// `now`.
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self {
            interval: DEFAULT_SPINNER_INTERVAL,
            last_advance: now,
        }
    }

    /// Construct with an explicit cadence (used in tests).
    #[must_use]
    pub fn with_interval(now: Instant, interval: Duration) -> Self {
        Self {
            interval,
            last_advance: now,
        }
    }

    /// Returns `true` iff at least `interval` has elapsed since the last
    /// advance. When `true`, the internal anchor is reset to `now` so the
    /// next call only reports `true` after another full interval.
    pub fn tick_at(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.last_advance) >= self.interval {
            self.last_advance = now;
            true
        } else {
            false
        }
    }

    /// Production entry point: ticks against `Instant::now()`.
    pub fn tick(&mut self) -> bool {
        self.tick_at(Instant::now())
    }
}

/// Classifies a crossterm event as a "terminal structural" notification
/// that needs a redraw reschedule but does NOT carry per-key state.
///
/// `None` means the event is not structural — the caller's existing
/// dispatch path (Key / Paste / Mouse / fallthrough) handles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalStructuralEvent {
    /// `Event::Resize(cols, rows)` — terminal window dimensions changed.
    Resize { cols: u16, rows: u16 },
    /// `Event::FocusGained` — terminal regained input focus.
    FocusGained,
    /// `Event::FocusLost` — terminal lost input focus.
    FocusLost,
}

/// Try to classify `event` as a [`TerminalStructuralEvent`]. Returns
/// `None` for all other event kinds (Key, Paste, Mouse, ...).
#[must_use]
pub fn classify_terminal_event(event: &CtEvent) -> Option<TerminalStructuralEvent> {
    match event {
        CtEvent::Resize(cols, rows) => Some(TerminalStructuralEvent::Resize {
            cols: *cols,
            rows: *rows,
        }),
        CtEvent::FocusGained => Some(TerminalStructuralEvent::FocusGained),
        CtEvent::FocusLost => Some(TerminalStructuralEvent::FocusLost),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn anchor() -> Instant {
        Instant::now()
    }

    #[test]
    fn timer_does_not_tick_before_interval() {
        let t0 = anchor();
        let mut timer = SpinnerTimer::with_interval(t0, Duration::from_millis(80));
        // 40ms elapsed → too soon.
        assert!(!timer.tick_at(t0 + Duration::from_millis(40)));
    }

    #[test]
    fn timer_ticks_when_interval_reached() {
        let t0 = anchor();
        let mut timer = SpinnerTimer::with_interval(t0, Duration::from_millis(80));
        assert!(timer.tick_at(t0 + Duration::from_millis(80)));
    }

    #[test]
    fn timer_resets_anchor_on_successful_tick() {
        let t0 = anchor();
        let mut timer = SpinnerTimer::with_interval(t0, Duration::from_millis(80));
        // First tick at +80ms succeeds and re-anchors.
        assert!(timer.tick_at(t0 + Duration::from_millis(80)));
        // +120ms is only +40ms after the new anchor → still too soon.
        assert!(!timer.tick_at(t0 + Duration::from_millis(120)));
        // +160ms is +80ms after the anchor → ticks.
        assert!(timer.tick_at(t0 + Duration::from_millis(160)));
    }

    #[test]
    fn timer_ticks_independently_of_terminal_events() {
        // The TASK #716 regression scenario: a sequence
        //   tick → tick → simulated resize → tick → tick
        // must yield FOUR spinner advances (one per tick boundary). The
        // resize event in the middle MUST NOT freeze the spinner. The
        // timer doesn't even know about events — that's the whole point
        // of the design: time-based, not event-based.
        let t0 = anchor();
        let interval = Duration::from_millis(80);
        let mut timer = SpinnerTimer::with_interval(t0, interval);

        let mut frame: u64 = 0;

        // Tick 1: +80ms.
        let t1 = t0 + Duration::from_millis(80);
        if timer.tick_at(t1) {
            frame += 1;
        }
        // Tick 2: +160ms.
        let t2 = t0 + Duration::from_millis(160);
        if timer.tick_at(t2) {
            frame += 1;
        }

        // Simulated terminal structural event at +200ms. Classify it so
        // the rest of the codebase can route it to the redraw queue.
        let resize_event = CtEvent::Resize(120, 40);
        let classified = classify_terminal_event(&resize_event);
        assert_eq!(
            classified,
            Some(TerminalStructuralEvent::Resize { cols: 120, rows: 40 })
        );
        // The classification is a pure inspection — it MUST NOT touch
        // the timer state. So the next two ticks behave exactly as if
        // the resize never happened.

        // Tick 3: +240ms (80ms after the +160 anchor).
        let t3 = t0 + Duration::from_millis(240);
        if timer.tick_at(t3) {
            frame += 1;
        }
        // Tick 4: +320ms.
        let t4 = t0 + Duration::from_millis(320);
        if timer.tick_at(t4) {
            frame += 1;
        }

        assert_eq!(
            frame, 4,
            "spinner frame must advance four times across the tick \
             sequence — the simulated resize at +200ms MUST NOT freeze \
             the timer (task #716)"
        );
    }

    #[test]
    fn timer_ticks_unaffected_by_focus_events() {
        // Same property as the resize test, but for FocusGained /
        // FocusLost. These are the other two structural events CC v2.1.145
        // called out.
        let t0 = anchor();
        let mut timer = SpinnerTimer::with_interval(t0, Duration::from_millis(80));

        // Two ticks.
        assert!(timer.tick_at(t0 + Duration::from_millis(80)));
        assert!(timer.tick_at(t0 + Duration::from_millis(160)));

        // Simulated focus loss + gain.
        let lost = CtEvent::FocusLost;
        let gained = CtEvent::FocusGained;
        assert_eq!(
            classify_terminal_event(&lost),
            Some(TerminalStructuralEvent::FocusLost)
        );
        assert_eq!(
            classify_terminal_event(&gained),
            Some(TerminalStructuralEvent::FocusGained)
        );

        // Two more ticks past the focus events.
        assert!(timer.tick_at(t0 + Duration::from_millis(240)));
        assert!(timer.tick_at(t0 + Duration::from_millis(320)));
    }

    #[test]
    fn classify_returns_none_for_non_structural_events() {
        // Key, Mouse, and Paste events are NOT structural — they have
        // their own dispatch paths in the input handler. The classifier
        // must report `None` for them so it doesn't shadow the existing
        // handling.
        let key = CtEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(classify_terminal_event(&key), None);

        let paste = CtEvent::Paste("hello".to_string());
        assert_eq!(classify_terminal_event(&paste), None);
    }
}
