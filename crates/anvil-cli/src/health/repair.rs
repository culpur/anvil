//! Repair dispatcher invoked by the `HealingModal`.
//!
//! The modal collects the user's selection (which probes to repair) and
//! hands it off to `repair_selected`.  Each probe's repair closure is
//! invoked in component-order, with every attempt logged + every failure
//! surfaced.  See feedback-no-silent-deferral — there is NO half-ship
//! path here.

use std::time::Instant;

use super::log;
use super::report::{Component, ProbeReport, ProbeResult, ProbeStatus, RepairFn};

/// Outcome of one repair attempt — surfaced back to the caller for UI
/// rendering (the modal shows a checklist of post-repair statuses).
#[derive(Debug, Clone)]
pub struct RepairOutcome {
    pub component: Component,
    pub success: bool,
    pub message: String,
    pub elapsed_ms: u64,
}

/// Dispatch repair for every selected component.
///
/// `selection` is the list of components the user ticked in the modal.
/// Components without a repair_fn are recorded as "skipped — no repair
/// closure attached" and surfaced in the outcome list.  Returns one
/// `RepairOutcome` per selected component.
pub fn repair_selected(
    report: &ProbeReport,
    selection: &[Component],
) -> Vec<RepairOutcome> {
    let issue_count = report
        .probes
        .iter()
        .filter(|p| matches!(p.status, ProbeStatus::Drift(_) | ProbeStatus::Broken(_)))
        .count();
    log::log_start(issue_count);

    // Build a one-line probe summary for the heal log.
    let probe_summary = report
        .probes
        .iter()
        .filter(|p| matches!(p.status, ProbeStatus::Drift(_) | ProbeStatus::Broken(_)))
        .map(|p| {
            format!(
                "{}:{}={}",
                p.component.as_str(),
                p.status.kind_str(),
                truncate_for_log(p.status.reason())
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    if !probe_summary.is_empty() {
        log::log_probe(&probe_summary);
    }

    let mut outcomes: Vec<RepairOutcome> = Vec::with_capacity(selection.len());

    for &component in selection {
        let Some(probe) = report.get(component) else {
            outcomes.push(RepairOutcome {
                component,
                success: false,
                message: format!(
                    "no probe result for {component:?} — was the report stale?"
                ),
                elapsed_ms: 0,
            });
            log::log_action(component.as_str(), "lookup", "failure", "no probe result");
            continue;
        };

        match invoke_repair(probe) {
            Ok(outcome) => outcomes.push(outcome),
            Err(e) => {
                let comp_str = component.as_str().to_string();
                log::log_action(&comp_str, "repair", "failure", &e);
                outcomes.push(RepairOutcome {
                    component,
                    success: false,
                    message: e,
                    elapsed_ms: 0,
                });
            }
        }
    }

    let all_repaired = outcomes.iter().all(|o| o.success);
    log::log_end(if all_repaired {
        "all repaired"
    } else if outcomes.iter().any(|o| o.success) {
        "partial — some repairs failed"
    } else {
        "all repairs failed"
    });

    outcomes
}

fn invoke_repair(probe: &ProbeResult) -> Result<RepairOutcome, String> {
    let comp_str = probe.component.as_str().to_string();

    let Some(repair) = probe.repair_fn.clone() else {
        log::log_action(&comp_str, "repair", "skipped", "no repair attached");
        return Ok(RepairOutcome {
            component: probe.component,
            success: false,
            message: format!(
                "No automatic repair available for {} (you can fix it manually).",
                probe.component.label()
            ),
            elapsed_ms: 0,
        });
    };

    let start = Instant::now();
    let result = run_repair_safely(&repair);
    let elapsed_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(msg) => {
            log::log_action(
                &comp_str,
                "repair",
                "success",
                &format!("took {elapsed_ms}ms"),
            );
            Ok(RepairOutcome {
                component: probe.component,
                success: true,
                message: msg,
                elapsed_ms,
            })
        }
        Err(e) => {
            log::log_action(
                &comp_str,
                "repair",
                "failure",
                &format!("{e} (took {elapsed_ms}ms)"),
            );
            Ok(RepairOutcome {
                component: probe.component,
                success: false,
                message: e,
                elapsed_ms,
            })
        }
    }
}

/// Invoke the repair closure with a `catch_unwind` shield.  Even though
/// closures should never panic, this contains the blast radius of a
/// misbehaving repair (we still want to log + surface — never silent
/// failure).
fn run_repair_safely(repair: &RepairFn) -> Result<String, String> {
    let repair_clone = repair.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || repair_clone()));
    match result {
        Ok(inner) => inner,
        Err(_) => Err("repair function panicked".to_string()),
    }
}

fn truncate_for_log(s: &str) -> String {
    // Single line, ≤ 60 chars for log readability.
    let one_line: String = s.chars().filter(|c| *c != '\n' && *c != '\r').collect();
    if one_line.len() > 60 {
        format!("{}…", &one_line[..57])
    } else {
        one_line
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn repair_selected_invokes_closures_in_order() {
        let counter = Arc::new(AtomicUsize::new(0));

        let c1 = Arc::clone(&counter);
        let r1: RepairFn = Arc::new(move || {
            c1.fetch_add(1, Ordering::SeqCst);
            Ok("vault perms reset".to_string())
        });

        let c2 = Arc::clone(&counter);
        let r2: RepairFn = Arc::new(move || {
            c2.fetch_add(10, Ordering::SeqCst);
            Ok("ollama started".to_string())
        });

        let report = ProbeReport::new(
            vec![
                ProbeResult::drift(Component::Vault, "perms".into(), 0).with_repair(r1),
                ProbeResult::broken(Component::Ollama, "down".into(), 0).with_repair(r2),
            ],
            0,
        );

        let outcomes =
            repair_selected(&report, &[Component::Vault, Component::Ollama]);
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().all(|o| o.success));
        assert_eq!(counter.load(Ordering::SeqCst), 11);
    }

    #[test]
    fn repair_selected_surfaces_failures() {
        let r: RepairFn = Arc::new(|| Err("oh no".to_string()));
        let report = ProbeReport::new(
            vec![ProbeResult::broken(Component::Ollama, "down".into(), 0).with_repair(r)],
            0,
        );
        let outcomes = repair_selected(&report, &[Component::Ollama]);
        assert_eq!(outcomes.len(), 1);
        assert!(!outcomes[0].success);
        assert_eq!(outcomes[0].message, "oh no");
    }

    #[test]
    fn repair_selected_handles_no_repair_attached() {
        let report = ProbeReport::new(
            vec![ProbeResult::drift(Component::Mcp, "missing".into(), 0)],
            0,
        );
        let outcomes = repair_selected(&report, &[Component::Mcp]);
        assert_eq!(outcomes.len(), 1);
        assert!(!outcomes[0].success);
        assert!(outcomes[0].message.contains("No automatic repair"));
    }

    #[test]
    fn repair_selected_catches_panics_from_closures() {
        let r: RepairFn = Arc::new(|| panic!("simulated repair panic"));
        let report = ProbeReport::new(
            vec![ProbeResult::broken(Component::Vault, "x".into(), 0).with_repair(r)],
            0,
        );
        let outcomes = repair_selected(&report, &[Component::Vault]);
        assert_eq!(outcomes.len(), 1);
        assert!(!outcomes[0].success);
        assert!(outcomes[0].message.contains("panicked"));
    }

    #[test]
    fn repair_selected_skips_components_not_in_selection() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&counter);
        let r: RepairFn = Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
            Ok("done".to_string())
        });
        let report = ProbeReport::new(
            vec![
                ProbeResult::drift(Component::Vault, "perms".into(), 0)
                    .with_repair(Arc::clone(&r)),
                ProbeResult::broken(Component::Ollama, "down".into(), 0).with_repair(r),
            ],
            0,
        );
        // Only select Vault.
        let outcomes = repair_selected(&report, &[Component::Vault]);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn truncate_for_log_handles_long_strings() {
        let s = "a".repeat(100);
        let truncated = truncate_for_log(&s);
        assert!(truncated.len() <= 60);
        assert!(truncated.ends_with('…'));
    }
}
