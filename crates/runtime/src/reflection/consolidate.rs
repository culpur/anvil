//! Reflective consolidation pass — Memory Cohesion Layer 6 (task #735).
//!
//! When the user invokes `/reflect consolidate` (or, in the future, when the
//! routines daemon triggers a session-boundary reflection), this module
//! scans the last N turns of the active session, surfaces patterns that
//! point at procedural memory the model should internalise, and writes a
//! markdown recap.
//!
//! The three patterns recognised today:
//!
//! * **Repeated-failure** — the same tool returned an error in `≥3`
//!   consecutive `ToolResult` blocks. Suggests the strategy is wrong, not
//!   that the tool is broken.
//! * **Repeated-success** — the same tool returned success in `≥5`
//!   consecutive `ToolResult` blocks. Worth capturing as a stable usage
//!   pattern (procedural memory candidate).
//! * **Strategy-switch** — the rolling stuck-detector has a non-empty
//!   `last_fired_turn_id`, indicating the runtime forced a pivot in the
//!   recent past. We surface it so the model knows a switch already
//!   happened.
//!
//! The output is intentionally a plain `String` (not JSON) so it can be
//! appended to `~/.anvil/daily/<date>.md` for the human-readable log.

use crate::session::{ContentBlock, ConversationMessage};

use super::stuck_detector::StuckDetector;

/// Default sliding window used when `/reflect consolidate` is invoked
/// without `--turns`. Matches the stuck-detector rolling window so the
/// two surfaces agree on "recent history".
pub const DEFAULT_CONSOLIDATE_WINDOW: usize = 20;

/// Threshold for the "repeated failure" pattern — `n` consecutive errors
/// from the same tool. Three is the same threshold the live stuck-detector
/// uses for `StuckPattern::ToolLoop`, but we count *any* identical-tool
/// errors (not identical-args), because consolidation is about strategy
/// learning, not real-time pivoting.
pub const REPEATED_FAILURE_THRESHOLD: usize = 3;

/// Threshold for the "stable usage" pattern — `n` consecutive successes
/// from the same tool. Higher than the failure threshold because we don't
/// want to spam recaps with mundane file reads.
pub const STABLE_USAGE_THRESHOLD: usize = 5;

/// Human-readable header prefix the daily-log writer keys on.
pub const RECAP_TAG: &str = "reflection";

/// One reflective finding plus optional follow-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub headline: String,
    pub suggestion: Option<String>,
}

/// Result of a single consolidation pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReflectionRecap {
    /// ISO-8601 date (`YYYY-MM-DD`) the recap was generated.
    pub date: String,
    /// How many session messages were scanned. Useful for debugging.
    pub turns_examined: usize,
    /// Findings in the order they were detected.
    pub findings: Vec<Finding>,
}

impl ReflectionRecap {
    /// `true` when no patterns were detected. Drives the
    /// "No patterns yet" footer in the markdown render.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }

    /// Render the recap as a Markdown block suitable for appending to the
    /// daily summary.  Always emits the `## Reflection — <date>` header so
    /// the daily-log reader can locate reflection blocks even when no
    /// findings were generated.
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("## Reflection - {}\n", self.date));
        out.push_str(&format!(
            "_examined {} message(s) from session history_\n\n",
            self.turns_examined
        ));

        if self.findings.is_empty() {
            out.push_str("No patterns yet.\n");
            return out;
        }

        out.push_str("### Findings\n");
        for f in &self.findings {
            out.push_str(&format!("- {}\n", f.headline));
        }

        let suggestions: Vec<&str> = self
            .findings
            .iter()
            .filter_map(|f| f.suggestion.as_deref())
            .collect();
        if !suggestions.is_empty() {
            out.push_str("\n### Suggestions\n");
            for s in &suggestions {
                out.push_str(&format!("- {s}\n"));
            }
        }

        out
    }
}

/// Iterate `messages` (limited to the last `window` entries) and detect
/// repeated-failure / stable-usage patterns.  If `detector` is supplied
/// and it last fired in the recent past, a strategy-switch finding is
/// appended.
///
/// The `date` parameter is injected (rather than computed inside) so the
/// caller can supply `daily::today_date()` in production and a fixed
/// string in tests — keeping this function pure.
#[must_use]
pub fn consolidate_session(
    messages: &[ConversationMessage],
    detector: Option<&StuckDetector>,
    window: usize,
    date: &str,
) -> ReflectionRecap {
    let window = window.max(1);
    let start = messages.len().saturating_sub(window);
    let slice = &messages[start..];

    let mut recap = ReflectionRecap {
        date: date.to_owned(),
        turns_examined: slice.len(),
        findings: Vec::new(),
    };

    // Flatten tool results in arrival order.  We don't care which message
    // a tool result was attached to — we just want the run of (tool,
    // is_error) pairs in time order.
    let mut tool_runs: Vec<(String, bool)> = Vec::new();
    for msg in slice {
        for block in &msg.blocks {
            if let ContentBlock::ToolResult { tool_name, is_error, .. } = block {
                tool_runs.push((tool_name.clone(), *is_error));
            }
        }
    }

    detect_repeated_failure(&tool_runs, &mut recap.findings);
    detect_stable_usage(&tool_runs, &mut recap.findings);
    detect_strategy_switch(detector, &mut recap.findings);

    recap
}

fn detect_repeated_failure(runs: &[(String, bool)], out: &mut Vec<Finding>) {
    let mut i = 0;
    while i < runs.len() {
        if !runs[i].1 {
            i += 1;
            continue;
        }
        let tool = runs[i].0.clone();
        let mut j = i;
        while j < runs.len() && runs[j].0 == tool && runs[j].1 {
            j += 1;
        }
        let run_len = j - i;
        if run_len >= REPEATED_FAILURE_THRESHOLD {
            out.push(Finding {
                headline: format!(
                    "repeated failure on {tool}: {run_len} consecutive errors"
                ),
                suggestion: Some(format!(
                    "consider switching strategy away from {tool} or capturing the failure mode as procedural memory"
                )),
            });
        }
        i = j;
    }
}

fn detect_stable_usage(runs: &[(String, bool)], out: &mut Vec<Finding>) {
    let mut i = 0;
    while i < runs.len() {
        if runs[i].1 {
            i += 1;
            continue;
        }
        let tool = runs[i].0.clone();
        let mut j = i;
        while j < runs.len() && runs[j].0 == tool && !runs[j].1 {
            j += 1;
        }
        let run_len = j - i;
        if run_len >= STABLE_USAGE_THRESHOLD {
            out.push(Finding {
                headline: format!(
                    "stable usage of {tool}: {run_len} consecutive successes"
                ),
                suggestion: None,
            });
        }
        i = j;
    }
}

fn detect_strategy_switch(detector: Option<&StuckDetector>, out: &mut Vec<Finding>) {
    let Some(det) = detector else { return };
    let Some(last_fired) = det.last_fired_turn_id() else {
        return;
    };
    let current = det.current_turn_id();
    // "Recent" = within the quiet window the detector itself uses.
    let recency = current.saturating_sub(last_fired);
    if recency > det.config().quiet_window_turns * 2 {
        return;
    }
    out.push(Finding {
        headline: format!(
            "strategy switch fired in turn {last_fired} (current turn {current})"
        ),
        suggestion: Some(
            "review reflection.scratchpad and the rolling window to capture the lesson"
                .to_owned(),
        ),
    });
}

/// Reflective daemon trigger - wired manually via `/reflect` for v2.2.19;
/// auto-fire is v3.x routines work (task #657). Always returns `None`
/// today so the cron driver can wire it without coupling to consolidation
/// state.
#[must_use]
pub fn reflect_daemon_tick(_state: &super::TurnState) -> Option<ReflectionRecap> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ContentBlock, ConversationMessage, MessageRole};

    fn tool_result_msg(tool: &str, is_error: bool) -> ConversationMessage {
        ConversationMessage {
            role: MessageRole::Tool,
            blocks: vec![ContentBlock::ToolResult {
                tool_use_id: "id".to_owned(),
                tool_name: tool.to_owned(),
                output: if is_error { "boom" } else { "ok" }.to_owned(),
                is_error,
            }],
            usage: None,
        }
    }

    #[test]
    fn consolidate_detects_repeated_failure() {
        let msgs = vec![
            tool_result_msg("Bash", true),
            tool_result_msg("Bash", true),
            tool_result_msg("Bash", true),
        ];
        let recap = consolidate_session(&msgs, None, 20, "2026-05-21");
        assert!(!recap.is_empty());
        let head = recap
            .findings
            .iter()
            .map(|f| f.headline.as_str())
            .collect::<Vec<_>>()
            .join("|");
        assert!(
            head.contains("repeated failure on Bash"),
            "expected repeated-failure headline, got: {head}"
        );
    }

    #[test]
    fn consolidate_detects_stable_usage() {
        let msgs = (0..5).map(|_| tool_result_msg("Read", false)).collect::<Vec<_>>();
        let recap = consolidate_session(&msgs, None, 20, "2026-05-21");
        let head = recap
            .findings
            .iter()
            .map(|f| f.headline.as_str())
            .collect::<Vec<_>>()
            .join("|");
        assert!(
            head.contains("stable usage of Read"),
            "expected stable-usage headline, got: {head}"
        );
    }

    #[test]
    fn consolidate_strategy_switch_detected_when_detector_fired_recently() {
        // Run the detector through 3 identical erroring Bash calls so it
        // fires ToolLoop, then check that consolidate surfaces the switch.
        use super::super::stuck_detector::{StuckDetector, ToolEvent};
        let mut det = StuckDetector::with_defaults();
        det.begin_turn();
        for _ in 0..3 {
            det.observe(ToolEvent::new("Bash", "x", Some("E".into()), None));
        }
        assert!(det.last_fired_turn_id().is_some(), "precondition: detector must have fired");

        let recap = consolidate_session(&[], Some(&det), 20, "2026-05-21");
        let head = recap
            .findings
            .iter()
            .map(|f| f.headline.as_str())
            .collect::<Vec<_>>()
            .join("|");
        assert!(
            head.contains("strategy switch fired"),
            "expected strategy-switch headline, got: {head}"
        );
    }

    #[test]
    fn consolidate_empty_history_returns_no_findings() {
        let recap = consolidate_session(&[], None, 20, "2026-05-21");
        assert!(recap.is_empty());
        let md = recap.render_markdown();
        assert!(md.contains("## Reflection - 2026-05-21"));
        assert!(md.contains("No patterns yet"));
    }

    #[test]
    fn consolidate_window_caps_examined_count() {
        let msgs = (0..100).map(|_| tool_result_msg("Read", false)).collect::<Vec<_>>();
        let recap = consolidate_session(&msgs, None, 5, "2026-05-21");
        assert_eq!(recap.turns_examined, 5);
    }

    #[test]
    fn render_markdown_contains_suggestions_when_present() {
        let msgs = vec![
            tool_result_msg("Bash", true),
            tool_result_msg("Bash", true),
            tool_result_msg("Bash", true),
        ];
        let recap = consolidate_session(&msgs, None, 20, "2026-05-21");
        let md = recap.render_markdown();
        assert!(md.contains("### Findings"));
        assert!(md.contains("### Suggestions"));
        assert!(md.contains("repeated failure on Bash"));
    }

    #[test]
    fn render_markdown_no_suggestions_section_when_none() {
        let msgs = (0..5).map(|_| tool_result_msg("Read", false)).collect::<Vec<_>>();
        let recap = consolidate_session(&msgs, None, 20, "2026-05-21");
        let md = recap.render_markdown();
        assert!(md.contains("### Findings"));
        // Stable-usage findings have no suggestion, so no suggestions section.
        assert!(
            !md.contains("### Suggestions"),
            "stable-usage-only recap should not emit suggestions, got:\n{md}"
        );
    }

    #[test]
    fn reflect_daemon_tick_is_none_until_routines_wire_it() {
        let ts = super::super::TurnState::with_defaults();
        assert!(reflect_daemon_tick(&ts).is_none());
    }

    #[test]
    fn run_must_be_contiguous_to_count() {
        // Bash err, Bash err, Read ok, Bash err — only a run of 2.
        let msgs = vec![
            tool_result_msg("Bash", true),
            tool_result_msg("Bash", true),
            tool_result_msg("Read", false),
            tool_result_msg("Bash", true),
        ];
        let recap = consolidate_session(&msgs, None, 20, "2026-05-21");
        let head = recap
            .findings
            .iter()
            .map(|f| f.headline.as_str())
            .collect::<Vec<_>>()
            .join("|");
        assert!(
            !head.contains("repeated failure on Bash"),
            "run was broken by an intervening Read; should not fire: {head}"
        );
    }
}
