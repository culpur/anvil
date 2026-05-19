//! Health probe + self-heal layer for Anvil (task #667, v2.2.18).
//!
//! Every existing-install boot of `anvil` runs `health::probe_all` in
//! Phase 0.5 (between "config.json exists" and "TUI launches").  Probes
//! detect drift / breakage in the install (vault perms wrong, Ollama daemon
//! down, QMD index stale, completions missing, …) and either heal silently
//! (drift) or surface a `HealingModal` for the user to confirm (breakage).
//!
//! Hard rules:
//! * Probes run in parallel with a HARD 200ms total-budget on the happy
//!   path.  Anything slower than 1500ms is cancelled (network probes).
//! * No `println!` / `print!` / `eprintln!` inside probes when the TUI is
//!   active (see feedback-tui-stdout-anti-pattern).  CLI-only `--check`
//!   path is fine — TUI hasn't been entered yet.
//! * Skip/defer state from #666 is respected.  See `setup_state`.
//! * Every repair attempt is logged + every failure surfaced (see
//!   feedback-no-silent-deferral).
//!
//! Wired in `main.rs::run_repl` between `anvil_config_json_exists() == true`
//! and the TUI launching.

// The probe layer publishes API surface (Component, Severity, RepairFn,
// LOCAL_FS_PROBE_BUDGET, NETWORK_PROBE_BUDGET, etc.) that A3/A4 will
// consume when their repair_with_progress hooks land.  Until then,
// some items are technically dead-code in this branch; we keep them
// because suppressing the warning is preferable to deleting the API
// other agents are about to call.
#![allow(dead_code)]

pub mod log;
pub mod modal;
pub mod probes;
pub mod report;
pub mod repair;
pub mod setup_state;

// Re-exports: the bare `show_healing_modal` requires an injected reader,
// so callers in main.rs go through `show_healing_modal_live` instead.  We
// keep both available so tests can target the injectable form directly.
pub use modal::{show_healing_modal, show_healing_modal_live, HealActionChoice};
pub use probes::probe_all;
pub use repair::repair_selected;
pub use report::{Component, ProbeReport, ProbeResult, ProbeStatus, RepairFn, Severity};

use std::time::Duration;

/// Hard total budget for the happy-path probe sweep.  Measured in
/// `probes::probe_all` and asserted in the timing test.
pub const HAPPY_PATH_BUDGET: Duration = Duration::from_millis(200);

/// Per-probe budget for network probes (DNS, OAuth, Ollama daemon).
pub const NETWORK_PROBE_BUDGET: Duration = Duration::from_millis(1500);

/// Per-probe budget for local filesystem probes.
pub const LOCAL_FS_PROBE_BUDGET: Duration = Duration::from_millis(500);
