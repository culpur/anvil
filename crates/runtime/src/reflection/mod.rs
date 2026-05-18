//! Autonomous reflection loop (task #636, v2.2.17 scope).
//!
//! When Anvil thinks it can't move forward it used to fail-hard.  This
//! module gives the runtime three orthogonal observation surfaces so it
//! can in-session stop, reason about why, and pivot - without taking the
//! autonomy axis away from the LLM (no automatic retry; we just inject a
//! system-reminder).

pub mod scratchpad;
pub mod strategy;
pub mod stuck_detector;

pub use scratchpad::{FailedAttempt, Scratchpad, SCRATCHPAD_CAPACITY};
pub use strategy::{summarize_failed_attempts, wrap_as_system_reminder};
pub use stuck_detector::{
    fnv1a64, StuckDetector, StuckDetectorConfig, StuckPattern, ToolEvent, THRASH_LOOKBACK,
    WINDOW_CAPACITY,
};

#[derive(Debug)]
pub struct TurnState {
    detector: StuckDetector,
    scratchpad: Scratchpad,
    pending_pattern: Option<StuckPattern>,
    enabled: bool,
}

impl TurnState {
    #[must_use]
    pub fn new(config: StuckDetectorConfig, enabled: bool) -> Self {
        Self {
            detector: StuckDetector::new(config),
            scratchpad: Scratchpad::new(),
            pending_pattern: None,
            enabled,
        }
    }

    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(StuckDetectorConfig::default(), true)
    }

    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn begin_turn(&mut self) {
        self.detector.begin_turn();
        self.scratchpad.clear();
        self.pending_pattern = None;
    }

    pub fn end_turn(&mut self) {
        self.scratchpad.clear();
    }

    pub fn observe_tool_event(&mut self, event: ToolEvent) {
        if !self.enabled {
            return;
        }
        if let Some(pat) = self.detector.observe(event) {
            otel_stuck_detected(&pat);
            self.pending_pattern = Some(pat);
        }
    }

    pub fn observe_inference_stall(&mut self, since_last: std::time::Duration) {
        if !self.enabled {
            return;
        }
        if let Some(pat) = self.detector.observe_inference_stall(since_last) {
            otel_stuck_detected(&pat);
            self.pending_pattern = Some(pat);
        }
    }

    pub fn record_failure(&mut self, tool: &str, args: &str, error: &str) {
        if !self.enabled {
            return;
        }
        self.scratchpad.push_from_post_tool_use(tool, args, error);
        otel_scratchpad_pushed(tool);
        otel_scratchpad_size(self.scratchpad.len());
    }

    #[must_use]
    pub const fn scratchpad(&self) -> &Scratchpad {
        &self.scratchpad
    }

    #[must_use]
    pub const fn detector(&self) -> &StuckDetector {
        &self.detector
    }

    #[must_use]
    pub const fn pending_pattern(&self) -> Option<&StuckPattern> {
        self.pending_pattern.as_ref()
    }

    pub fn take_pending_pattern(&mut self) -> Option<StuckPattern> {
        self.pending_pattern.take()
    }

    pub fn drain_reminder_for_next_call(&mut self) -> String {
        if !self.enabled {
            return String::new();
        }
        let mut blocks: Vec<String> = Vec::new();

        if let Some(pat) = self.take_pending_pattern() {
            otel_strategy_switch(&pat);
            let body = summarize_failed_attempts(Some(&pat), self.detector.window());
            let wrapped = wrap_as_system_reminder(&body);
            if !wrapped.is_empty() {
                blocks.push(wrapped);
            }
        }

        let scratch_body = self.scratchpad.render_reminder_body();
        let scratch_wrapped = wrap_as_system_reminder(&scratch_body);
        if !scratch_wrapped.is_empty() {
            blocks.push(scratch_wrapped);
        }

        blocks.join("\n\n")
    }
}

impl Default for TurnState {
    fn default() -> Self {
        Self::with_defaults()
    }
}

pub fn otel_stuck_detected(pat: &StuckPattern) {
    crate::otel::emit_event(
        "reflection.stuck_detected",
        &[("pattern", pat.label())],
    );
}

pub fn otel_strategy_switch(pat: &StuckPattern) {
    crate::otel::emit_event(
        "reflection.strategy_switch",
        &[("pattern", pat.label())],
    );
}

pub fn otel_scratchpad_pushed(tool: &str) {
    crate::otel::emit_event(
        "reflection.scratchpad_pushed",
        &[("tool", tool)],
    );
}

pub fn otel_scratchpad_size(size: usize) {
    let buf = size.to_string();
    crate::otel::emit_event(
        "reflection.scratchpad_size",
        &[("size", buf.as_str())],
    );
}

pub fn otel_user_invoked() {
    crate::otel::emit_event("reflection.user_invoked", &[]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn turnstate_disabled_is_inert() {
        let mut ts = TurnState::new(StuckDetectorConfig::default(), false);
        ts.begin_turn();
        for _ in 0..5 {
            ts.observe_tool_event(ToolEvent::new("Bash", "x", Some("E".into()), None));
        }
        ts.record_failure("Bash", "x", "E");
        assert!(ts.pending_pattern().is_none());
        assert!(ts.scratchpad().is_empty());
        assert_eq!(ts.drain_reminder_for_next_call(), "");
    }

    #[test]
    fn turnstate_enabled_drains_strategy_and_scratchpad_blocks() {
        let mut ts = TurnState::with_defaults();
        ts.begin_turn();
        for _ in 0..3 {
            ts.observe_tool_event(ToolEvent::new("Bash", "x", Some("E".into()), None));
        }
        for _ in 0..2 {
            ts.record_failure("Bash", "x", "E");
        }
        assert!(ts.pending_pattern().is_some());
        let reminder = ts.drain_reminder_for_next_call();
        assert!(
            reminder.contains("<system-reminder>"),
            "must emit wrapped reminder, got: {reminder}"
        );
        assert!(reminder.contains("Stuck pattern detected"));
        assert!(reminder.contains("Previously tried in this turn"));
        assert!(ts.pending_pattern().is_none());
    }

    #[test]
    fn turnstate_clears_scratchpad_at_end_turn() {
        let mut ts = TurnState::with_defaults();
        ts.begin_turn();
        ts.record_failure("Bash", "x", "E");
        ts.record_failure("Edit", "y", "F");
        assert_eq!(ts.scratchpad().len(), 2);
        ts.end_turn();
        assert!(ts.scratchpad().is_empty());
    }

    #[test]
    fn turnstate_observes_inference_stall() {
        let mut ts = TurnState::with_defaults();
        ts.begin_turn();
        ts.observe_inference_stall(Duration::from_secs(900));
        assert!(matches!(
            ts.pending_pattern(),
            Some(StuckPattern::InferenceStall { secs }) if *secs == 900
        ));
    }

    #[test]
    fn turnstate_oscillation_fires_on_repeated_edits() {
        let mut ts = TurnState::with_defaults();
        ts.begin_turn();
        let path = PathBuf::from("/tmp/foo.rs");
        for _ in 0..3 {
            ts.observe_tool_event(ToolEvent::new(
                "Edit",
                "edit",
                None,
                Some(path.clone()),
            ));
        }
        assert!(matches!(
            ts.pending_pattern(),
            Some(StuckPattern::Oscillation { .. })
        ));
    }

    #[test]
    fn turnstate_no_reminder_when_no_failures() {
        let mut ts = TurnState::with_defaults();
        ts.begin_turn();
        ts.observe_tool_event(ToolEvent::new("Bash", "ls", None, None));
        assert!(ts.pending_pattern().is_none());
        assert!(ts.scratchpad().is_empty());
        assert_eq!(ts.drain_reminder_for_next_call(), "");
    }
}
