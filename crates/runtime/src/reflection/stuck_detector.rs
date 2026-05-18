//! Stuck-pattern detection for the autonomous reflection loop (task #636).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

pub const WINDOW_CAPACITY: usize = 20;
pub const THRASH_LOOKBACK: usize = 5;
pub const DEFAULT_THRASH_ERROR_RATE: f32 = 0.5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolEvent {
    pub tool_name: String,
    pub args_hash: u64,
    pub error: Option<String>,
    pub touched_file: Option<PathBuf>,
    pub timestamp: SystemTime,
}

impl ToolEvent {
    #[must_use]
    pub fn new(
        tool_name: impl Into<String>,
        args: &str,
        error: Option<String>,
        touched_file: Option<PathBuf>,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            args_hash: fnv1a64(args.as_bytes()),
            error: error.map(|e| {
                if e.len() > 200 {
                    let mut t = e.chars().take(200).collect::<String>();
                    t.push('\u{2026}');
                    t
                } else {
                    e
                }
            }),
            touched_file,
            timestamp: SystemTime::now(),
        }
    }

    #[must_use]
    pub const fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StuckPattern {
    ToolLoop { tool: String, count: u32 },
    Thrashing { error_rate: f32 },
    InferenceStall { secs: u64 },
    Oscillation { file: PathBuf, edits: u32 },
}

impl StuckPattern {
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::ToolLoop { .. } => "tool_loop",
            Self::Thrashing { .. } => "thrashing",
            Self::InferenceStall { .. } => "inference_stall",
            Self::Oscillation { .. } => "oscillation",
        }
    }

    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::ToolLoop { tool, count } => {
                format!("repeated {count} identical {tool} calls with the same error")
            }
            Self::Thrashing { error_rate } => {
                format!("{:.0}% tool-error rate over the last {} calls", error_rate * 100.0, THRASH_LOOKBACK)
            }
            Self::InferenceStall { secs } => {
                format!("inference stalled for {secs}s with no tool call or completion")
            }
            Self::Oscillation { file, edits } => {
                format!("{edits} edits to the same file ({}) within this turn", file.display())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StuckDetectorConfig {
    pub stuck_threshold_calls: u32,
    pub thrash_error_rate: f32,
    pub stall_timeout_secs: u64,
    pub quiet_window_turns: u32,
    pub oscillation_threshold: u32,
}

impl Default for StuckDetectorConfig {
    fn default() -> Self {
        Self {
            stuck_threshold_calls: 3,
            thrash_error_rate: DEFAULT_THRASH_ERROR_RATE,
            stall_timeout_secs: 600,
            quiet_window_turns: 3,
            oscillation_threshold: 3,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StuckDetector {
    window: VecDeque<ToolEvent>,
    config: StuckDetectorConfig,
    current_turn_id: u32,
    last_fired_turn_id: Option<u32>,
    edits_this_turn: std::collections::HashMap<PathBuf, u32>,
}

impl StuckDetector {
    #[must_use]
    pub fn new(config: StuckDetectorConfig) -> Self {
        Self {
            window: VecDeque::with_capacity(WINDOW_CAPACITY),
            config,
            current_turn_id: 0,
            last_fired_turn_id: None,
            edits_this_turn: std::collections::HashMap::new(),
        }
    }

    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(StuckDetectorConfig::default())
    }

    pub fn begin_turn(&mut self) {
        self.current_turn_id = self.current_turn_id.wrapping_add(1);
        self.edits_this_turn.clear();
    }

    #[must_use]
    pub fn window(&self) -> &VecDeque<ToolEvent> {
        &self.window
    }

    /// Read-only view of the detector's compiled-in config. Used by the
    /// config-tests and by `/reflect` introspection to verify the
    /// settings.json overrides reached the detector.
    #[must_use]
    pub const fn config(&self) -> &StuckDetectorConfig {
        &self.config
    }

    #[must_use]
    pub const fn current_turn_id(&self) -> u32 {
        self.current_turn_id
    }

    #[must_use]
    pub const fn last_fired_turn_id(&self) -> Option<u32> {
        self.last_fired_turn_id
    }

    #[must_use]
    pub const fn is_quiet(&self) -> bool {
        match self.last_fired_turn_id {
            Some(last) => self.current_turn_id < last + self.config.quiet_window_turns,
            None => false,
        }
    }

    pub fn observe(&mut self, event: ToolEvent) -> Option<StuckPattern> {
        if let Some(ref f) = event.touched_file {
            *self.edits_this_turn.entry(f.clone()).or_insert(0) += 1;
        }

        if self.window.len() == WINDOW_CAPACITY {
            self.window.pop_front();
        }
        self.window.push_back(event);

        if self.is_quiet() {
            return None;
        }

        if let Some(p) = self.detect_tool_loop() {
            self.fire(p)
        } else if let Some(p) = self.detect_oscillation() {
            self.fire(p)
        } else if let Some(p) = self.detect_thrashing() {
            self.fire(p)
        } else {
            None
        }
    }

    pub fn observe_inference_stall(&mut self, since_last: Duration) -> Option<StuckPattern> {
        if self.is_quiet() {
            return None;
        }
        let secs = since_last.as_secs();
        if secs >= self.config.stall_timeout_secs {
            self.fire(StuckPattern::InferenceStall { secs })
        } else {
            None
        }
    }

    fn fire(&mut self, pattern: StuckPattern) -> Option<StuckPattern> {
        self.last_fired_turn_id = Some(self.current_turn_id);
        Some(pattern)
    }

    fn detect_tool_loop(&self) -> Option<StuckPattern> {
        let n = self.config.stuck_threshold_calls as usize;
        if n == 0 || self.window.len() < n {
            return None;
        }
        let tail = self.window.iter().rev().take(n).collect::<Vec<_>>();
        let first = tail.first()?;
        if !first.is_error() {
            return None;
        }
        let all_match = tail.iter().all(|ev| {
            ev.is_error()
                && ev.tool_name == first.tool_name
                && ev.args_hash == first.args_hash
                && ev.error == first.error
        });
        if all_match {
            Some(StuckPattern::ToolLoop {
                tool: first.tool_name.clone(),
                count: n as u32,
            })
        } else {
            None
        }
    }

    fn detect_thrashing(&self) -> Option<StuckPattern> {
        if self.window.len() < THRASH_LOOKBACK {
            return None;
        }
        let recent = self
            .window
            .iter()
            .rev()
            .take(THRASH_LOOKBACK)
            .collect::<Vec<_>>();
        let errors = recent.iter().filter(|e| e.is_error()).count();
        let rate = errors as f32 / THRASH_LOOKBACK as f32;
        if rate > self.config.thrash_error_rate {
            Some(StuckPattern::Thrashing { error_rate: rate })
        } else {
            None
        }
    }

    fn detect_oscillation(&self) -> Option<StuckPattern> {
        let threshold = self.config.oscillation_threshold;
        if threshold == 0 {
            return None;
        }
        for (file, count) in &self.edits_this_turn {
            if *count >= threshold {
                return Some(StuckPattern::Oscillation {
                    file: file.clone(),
                    edits: *count,
                });
            }
        }
        None
    }
}

impl Default for StuckDetector {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[must_use]
pub const fn fnv1a64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn err(msg: &str) -> Option<String> {
        Some(msg.to_string())
    }

    #[test]
    fn tool_loop_triggers_after_3_identical_calls() {
        let mut det = StuckDetector::with_defaults();
        det.begin_turn();
        let e = || ToolEvent::new("Bash", "ls /missing", err("ENOENT"), None);
        assert!(det.observe(e()).is_none());
        assert!(det.observe(e()).is_none());
        let third = det.observe(e());
        assert!(
            matches!(third, Some(StuckPattern::ToolLoop { ref tool, count: 3 }) if tool == "Bash"),
            "expected ToolLoop on 3rd identical erroring call, got {third:?}"
        );
    }

    #[test]
    fn tool_loop_does_not_trigger_on_success() {
        let mut det = StuckDetector::with_defaults();
        det.begin_turn();
        let e = || ToolEvent::new("Bash", "ls", None, None);
        det.observe(e());
        det.observe(e());
        let third = det.observe(e());
        assert!(third.is_none(), "successful repeats are not stuck");
    }

    #[test]
    fn thrashing_triggers_at_50pct_error_rate() {
        let cfg = StuckDetectorConfig {
            stuck_threshold_calls: 99,
            ..Default::default()
        };
        let mut det = StuckDetector::new(cfg);
        det.begin_turn();
        det.observe(ToolEvent::new("A", "x", None, None));
        det.observe(ToolEvent::new("B", "y", err("e1"), None));
        det.observe(ToolEvent::new("C", "z", err("e2"), None));
        det.observe(ToolEvent::new("D", "w", None, None));
        let last = det.observe(ToolEvent::new("E", "v", err("e3"), None));
        match last {
            Some(StuckPattern::Thrashing { error_rate }) => {
                assert!(
                    (error_rate - 0.6).abs() < 0.01,
                    "expected ~60% error rate, got {error_rate}"
                );
            }
            other => panic!("expected Thrashing, got {other:?}"),
        }
    }

    #[test]
    fn thrashing_quiet_under_threshold() {
        let cfg = StuckDetectorConfig {
            stuck_threshold_calls: 99,
            ..Default::default()
        };
        let mut det = StuckDetector::new(cfg);
        det.begin_turn();
        det.observe(ToolEvent::new("A", "x", err("e"), None));
        det.observe(ToolEvent::new("B", "y", None, None));
        det.observe(ToolEvent::new("C", "z", None, None));
        det.observe(ToolEvent::new("D", "w", None, None));
        let last = det.observe(ToolEvent::new("E", "v", None, None));
        assert!(last.is_none(), "20% error rate is below 50% threshold");
    }

    #[test]
    fn inference_stall_triggers_at_threshold() {
        let mut det = StuckDetector::with_defaults();
        det.begin_turn();
        assert!(
            det.observe_inference_stall(Duration::from_secs(599)).is_none(),
            "below threshold"
        );
        let hit = det.observe_inference_stall(Duration::from_secs(601));
        assert!(
            matches!(hit, Some(StuckPattern::InferenceStall { secs }) if secs == 601),
            "expected stall above 600s, got {hit:?}"
        );
    }

    #[test]
    fn oscillation_triggers_at_3_edits_same_file() {
        let mut det = StuckDetector::with_defaults();
        det.begin_turn();
        let path = PathBuf::from("/tmp/foo.rs");
        let e = || ToolEvent::new("Edit", "edit foo.rs", None, Some(path.clone()));
        assert!(det.observe(e()).is_none());
        assert!(det.observe(e()).is_none());
        let third = det.observe(e());
        match third {
            Some(StuckPattern::Oscillation { ref file, edits: 3 }) => {
                assert_eq!(file, &path);
            }
            other => panic!("expected Oscillation on 3rd edit, got {other:?}"),
        }
    }

    #[test]
    fn oscillation_resets_at_turn_boundary() {
        let mut det = StuckDetector::with_defaults();
        det.begin_turn();
        let path = PathBuf::from("/tmp/foo.rs");
        let e = || ToolEvent::new("Edit", "edit", None, Some(path.clone()));
        det.observe(e());
        det.observe(e());
        det.begin_turn();
        let after_reset = det.observe(e());
        assert!(
            after_reset.is_none(),
            "edit count resets across turns; got {after_reset:?}"
        );
    }

    #[test]
    fn quiet_window_suppresses_refires() {
        let mut det = StuckDetector::with_defaults();
        det.begin_turn();
        let e = || ToolEvent::new("Bash", "x", err("E"), None);
        det.observe(e());
        det.observe(e());
        let fired = det.observe(e());
        assert!(matches!(fired, Some(StuckPattern::ToolLoop { .. })));
        assert!(det.last_fired_turn_id().is_some());

        det.begin_turn();
        let e2 = ToolEvent::new("Edit", "y", err("E2"), Some(PathBuf::from("/x")));
        let suppressed_1 = det.observe(e2.clone());
        assert!(suppressed_1.is_none(), "turn +1 must be quiet");

        det.begin_turn();
        let suppressed_2 = det.observe(e2.clone());
        assert!(suppressed_2.is_none(), "turn +2 must be quiet");

        det.begin_turn();
        det.begin_turn();
        assert!(!det.is_quiet(), "quiet window must have elapsed");
    }

    #[test]
    fn window_caps_at_capacity() {
        let mut det = StuckDetector::with_defaults();
        det.begin_turn();
        for i in 0..(WINDOW_CAPACITY + 10) {
            det.observe(ToolEvent::new("X", &format!("{i}"), None, None));
        }
        assert_eq!(det.window().len(), WINDOW_CAPACITY);
    }

    #[test]
    fn fnv1a64_is_stable() {
        assert_eq!(fnv1a64(b""), 0xcbf29ce484222325);
        assert_ne!(fnv1a64(b"abc"), fnv1a64(b"abd"));
        assert_eq!(fnv1a64(b"hello"), fnv1a64(b"hello"));
    }
}
