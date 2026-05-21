//! Structured report types for the health-probe layer.
//!
//! `ProbeReport` is the top-level result of `probes::probe_all`.  It carries
//! one `ProbeResult` per component the probe layer knows about, plus an
//! aggregate `Severity` that drives the Phase 0.5 routing logic in main.rs.

use std::fmt;
use std::sync::Arc;

/// Closure invoked by the `HealingModal` to repair a single probe's finding.
///
/// Repair functions are *owned* by the `ProbeResult` (not the probe code)
/// so that the modal can fire them after the user confirms.  Returns a
/// short human-readable message describing the outcome, suitable for the
/// heal log + a TUI system line.
///
/// Repair functions run AFTER the alt-screen has been left (we render the
/// modal in raw mode, but actual install/repair work happens on the main
/// thread with the screensaver suspended), so they are allowed to do I/O.
/// They MUST NOT panic — propagate errors via the `Err` arm and the caller
/// will log + surface.
pub type RepairFn = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

/// Components the health probe layer knows about.
///
/// One-to-one with the probe functions in `probes/`.  When a new probe is
/// added, add a new variant here AND extend the `as_str` impl + the
/// `HealingModal` rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Component {
    Config,
    Vault,
    Provider,
    Ollama,
    Qmd,
    Filesystem,
    Completions,
    Binary,
    Mcp,
    Daemon,
}

impl Component {
    /// Stable identifier used in heal logs and CLI output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Vault => "vault",
            Self::Provider => "provider",
            Self::Ollama => "ollama",
            Self::Qmd => "qmd",
            Self::Filesystem => "filesystem",
            Self::Completions => "completions",
            Self::Binary => "binary",
            Self::Mcp => "mcp",
            Self::Daemon => "daemon",
        }
    }

    /// Human-friendly label rendered in the modal.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Config => "Config",
            Self::Vault => "Vault",
            Self::Provider => "Default provider auth",
            Self::Ollama => "Ollama",
            Self::Qmd => "QMD",
            Self::Filesystem => "Filesystem",
            Self::Completions => "Shell completions",
            Self::Binary => "Anvil binary",
            Self::Mcp => "MCP servers",
            Self::Daemon => "Daemon",
        }
    }

    /// All components, in the order rendered by the modal + CLI report.
    pub const ALL: &'static [Self] = &[
        Self::Config,
        Self::Vault,
        Self::Provider,
        Self::Ollama,
        Self::Qmd,
        Self::Filesystem,
        Self::Completions,
        Self::Binary,
        Self::Mcp,
        Self::Daemon,
    ];
}

/// Per-probe status.  Severity ordering:
/// `Healthy < NotApplicable < Drift < Broken`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeStatus {
    /// Component is operating normally — silent, continue.
    Healthy,
    /// Component is functional but degraded.  Repair recommended; rail
    /// nudge only.  Examples: bench > 30 days old, OAuth token expires
    /// in < 5 min.
    Drift(String),
    /// Component is broken.  Modal surfaces, repair is offered.
    Broken(String),
    /// Component is not applicable to this install (e.g. Ollama probe
    /// when `ollama.enabled=false`).  Silent.
    NotApplicable(String),
}

impl ProbeStatus {
    /// Numeric severity, where higher = worse.  Used by `Severity::worst`.
    #[must_use]
    pub const fn rank(&self) -> u8 {
        match self {
            Self::Healthy => 0,
            Self::NotApplicable(_) => 0,
            Self::Drift(_) => 1,
            Self::Broken(_) => 2,
        }
    }

    /// Stable identifier for log lines (`probe.status=X`).
    #[must_use]
    pub const fn kind_str(&self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::NotApplicable(_) => "na",
            Self::Drift(_) => "drift",
            Self::Broken(_) => "broken",
        }
    }

    /// Reason text or empty.
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Healthy => "",
            Self::Drift(r) | Self::Broken(r) | Self::NotApplicable(r) => r.as_str(),
        }
    }
}

/// Aggregate severity rolled up from every probe result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// All probes Healthy or NotApplicable — silent boot.
    Green,
    /// At least one probe in Drift — rail nudge, continue.
    Drift,
    /// At least one probe in Broken — modal surfaces, repair offered.
    Breakage,
}

impl Severity {
    /// Worst-of-all aggregation rule.
    #[must_use]
    pub const fn from_rank(rank: u8) -> Self {
        match rank {
            0 => Self::Green,
            1 => Self::Drift,
            _ => Self::Breakage,
        }
    }

    /// Stable identifier for log lines.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Drift => "drift",
            Self::Breakage => "breakage",
        }
    }
}

/// One probe's result.  Pair of (status, optional repair closure).
#[derive(Clone)]
pub struct ProbeResult {
    pub component: Component,
    pub status: ProbeStatus,
    pub repair_fn: Option<RepairFn>,
    /// Time the probe took to run.  Reported in `--check` output and used
    /// by the timing test.
    pub elapsed_ms: u64,
}

impl fmt::Debug for ProbeResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProbeResult")
            .field("component", &self.component)
            .field("status", &self.status)
            .field("repair_fn", &self.repair_fn.is_some())
            .field("elapsed_ms", &self.elapsed_ms)
            .finish()
    }
}

impl PartialEq for ProbeResult {
    fn eq(&self, other: &Self) -> bool {
        // Ignore repair_fn (closures aren't Eq).
        self.component == other.component
            && self.status == other.status
            && self.elapsed_ms == other.elapsed_ms
    }
}

impl ProbeResult {
    #[must_use]
    pub const fn healthy(component: Component, elapsed_ms: u64) -> Self {
        Self {
            component,
            status: ProbeStatus::Healthy,
            repair_fn: None,
            elapsed_ms,
        }
    }

    #[must_use]
    pub const fn not_applicable(component: Component, reason: String, elapsed_ms: u64) -> Self {
        Self {
            component,
            status: ProbeStatus::NotApplicable(reason),
            repair_fn: None,
            elapsed_ms,
        }
    }

    #[must_use]
    pub const fn drift(component: Component, reason: String, elapsed_ms: u64) -> Self {
        Self {
            component,
            status: ProbeStatus::Drift(reason),
            repair_fn: None,
            elapsed_ms,
        }
    }

    #[must_use]
    pub const fn broken(component: Component, reason: String, elapsed_ms: u64) -> Self {
        Self {
            component,
            status: ProbeStatus::Broken(reason),
            repair_fn: None,
            elapsed_ms,
        }
    }

    #[must_use]
    pub fn with_repair(mut self, repair: RepairFn) -> Self {
        self.repair_fn = Some(repair);
        self
    }

    #[must_use]
    pub fn needs_attention(&self) -> bool {
        matches!(self.status, ProbeStatus::Drift(_) | ProbeStatus::Broken(_))
    }

    #[must_use]
    pub fn is_broken(&self) -> bool {
        matches!(self.status, ProbeStatus::Broken(_))
    }

    #[must_use]
    pub fn is_drifted(&self) -> bool {
        matches!(self.status, ProbeStatus::Drift(_))
    }
}

/// Top-level result of `probes::probe_all`.
#[derive(Debug, Clone)]
pub struct ProbeReport {
    pub probes: Vec<ProbeResult>,
    /// Total wall time of the probe sweep.  Asserted ≤ HAPPY_PATH_BUDGET
    /// on a healthy install by the timing test.
    pub total_elapsed_ms: u64,
}

impl ProbeReport {
    #[must_use]
    pub fn new(probes: Vec<ProbeResult>, total_elapsed_ms: u64) -> Self {
        Self {
            probes,
            total_elapsed_ms,
        }
    }

    /// Worst-of-all severity across every probe result.
    #[must_use]
    pub fn severity(&self) -> Severity {
        let worst = self
            .probes
            .iter()
            .map(|p| p.status.rank())
            .max()
            .unwrap_or(0);
        Severity::from_rank(worst)
    }

    #[must_use]
    pub fn broken(&self) -> Vec<&ProbeResult> {
        self.probes.iter().filter(|p| p.is_broken()).collect()
    }

    #[must_use]
    pub fn drifted(&self) -> Vec<&ProbeResult> {
        self.probes.iter().filter(|p| p.is_drifted()).collect()
    }

    /// Lookup a single component's result by name (CLI/test helper).
    #[must_use]
    pub fn get(&self, component: Component) -> Option<&ProbeResult> {
        self.probes.iter().find(|p| p.component == component)
    }

    /// Render the report as a structured plain-text block suitable for
    /// `anvil --check` stdout.  Caller is responsible for printing —
    /// this is allowed to use plain strings (no TUI active in `--check`).
    #[must_use]
    pub fn render_cli(&self) -> String {
        use rust_i18n::t;
        let mut out = String::new();
        out.push_str(&t!("slash.heal.title"));
        out.push('\n');
        out.push_str(&"─".repeat(60));
        out.push('\n');
        for probe in &self.probes {
            let (icon, label_color, reset) = match probe.status {
                ProbeStatus::Healthy => ("✓", "\x1b[32m", "\x1b[0m"),
                ProbeStatus::NotApplicable(_) => ("·", "\x1b[90m", "\x1b[0m"),
                ProbeStatus::Drift(_) => ("⚠", "\x1b[33m", "\x1b[0m"),
                ProbeStatus::Broken(_) => ("✗", "\x1b[31m", "\x1b[0m"),
            };
            let reason = probe.status.reason();
            let reason_str = if reason.is_empty() {
                String::new()
            } else {
                format!(" — {reason}")
            };
            out.push_str(&format!(
                "  {label_color}{icon}{reset}  {:<22}{reason_str}\n",
                probe.component.label()
            ));
        }
        out.push_str(&"─".repeat(60));
        out.push('\n');
        let severity = self.severity();
        let line = match severity {
            Severity::Green => format!("  \x1b[1;32m{}\x1b[0m\n", t!("slash.heal.all_healthy")),
            Severity::Drift => format!("  \x1b[1;33m{}\x1b[0m\n", t!("slash.heal.drift_detected")),
            Severity::Breakage => format!("  \x1b[1;31m{}\x1b[0m\n", t!("slash.heal.breakage_detected")),
        };
        out.push_str(&line);
        out.push_str(&format!("  {}\n", t!("slash.heal.sweep_took", ms = self.total_elapsed_ms)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_rank_ordering() {
        assert!(ProbeStatus::Healthy.rank() < ProbeStatus::Drift("x".into()).rank());
        assert!(ProbeStatus::Drift("x".into()).rank() < ProbeStatus::Broken("x".into()).rank());
        assert_eq!(
            ProbeStatus::NotApplicable("x".into()).rank(),
            ProbeStatus::Healthy.rank()
        );
    }

    #[test]
    fn report_severity_is_worst_of_all() {
        let probes = vec![
            ProbeResult::healthy(Component::Config, 1),
            ProbeResult::drift(Component::Qmd, "stale".into(), 1),
            ProbeResult::broken(Component::Ollama, "down".into(), 1),
        ];
        let report = ProbeReport::new(probes, 3);
        assert_eq!(report.severity(), Severity::Breakage);
        assert_eq!(report.broken().len(), 1);
        assert_eq!(report.drifted().len(), 1);
    }

    #[test]
    fn report_severity_drift_when_no_break() {
        let probes = vec![
            ProbeResult::healthy(Component::Config, 1),
            ProbeResult::drift(Component::Qmd, "stale".into(), 1),
        ];
        let report = ProbeReport::new(probes, 2);
        assert_eq!(report.severity(), Severity::Drift);
    }

    #[test]
    fn report_severity_green_when_all_healthy_or_na() {
        let probes = vec![
            ProbeResult::healthy(Component::Config, 1),
            ProbeResult::not_applicable(Component::Ollama, "disabled".into(), 1),
        ];
        let report = ProbeReport::new(probes, 2);
        assert_eq!(report.severity(), Severity::Green);
    }

    #[test]
    fn cli_render_contains_status_lines() {
        let report = ProbeReport::new(
            vec![
                ProbeResult::healthy(Component::Config, 5),
                ProbeResult::broken(Component::Ollama, "daemon down".into(), 12),
            ],
            17,
        );
        let s = report.render_cli();
        assert!(s.contains("Config"));
        assert!(s.contains("Ollama"));
        assert!(s.contains("daemon down"));
        assert!(s.contains("17ms"));
    }

    #[test]
    fn component_label_stable() {
        // Every component must have a non-empty stable identifier + label.
        for comp in Component::ALL {
            assert!(!comp.as_str().is_empty());
            assert!(!comp.label().is_empty());
        }
    }

    #[test]
    fn probe_result_needs_attention() {
        assert!(!ProbeResult::healthy(Component::Config, 0).needs_attention());
        assert!(!ProbeResult::not_applicable(Component::Ollama, "off".into(), 0).needs_attention());
        assert!(ProbeResult::drift(Component::Qmd, "stale".into(), 0).needs_attention());
        assert!(ProbeResult::broken(Component::Ollama, "down".into(), 0).needs_attention());
    }

    #[test]
    fn severity_from_rank() {
        assert_eq!(Severity::from_rank(0), Severity::Green);
        assert_eq!(Severity::from_rank(1), Severity::Drift);
        assert_eq!(Severity::from_rank(2), Severity::Breakage);
        assert_eq!(Severity::from_rank(99), Severity::Breakage);
    }
}
