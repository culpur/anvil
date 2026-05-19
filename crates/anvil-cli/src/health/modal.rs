//! HealingModal — the Phase 0.5 user-facing repair prompt.
//!
//! Renders BEFORE the TUI alt-screen is entered, so plain stdout/stdin is
//! allowed (no feedback-tui-stdout-anti-pattern violation).  When A1's
//! `HealthProbeModal` ratatui primitive lands, this can switch to
//! rendering inside the existing terminal session — for now, plain
//! line-mode keeps the modal usable on every platform without a TUI
//! dependency.
//!
//! User flow:
//! 1. Show the checklist of probes with [✓] [⚠] [✗] icons.
//! 2. Each repairable issue gets a `[idx]` marker.
//! 3. Read a single response line:
//!      - "r N M …"  → repair selected indices
//!      - "a"        → repair all
//!      - "c"        → continue without repairing (rail nudge later)
//!      - "q"        → quit Anvil
//!      - ""         → same as `c` (default = continue)
//! 4. Hand selection off to `repair::repair_selected`.

use std::io::{self, BufRead, IsTerminal, Write};

use super::report::{Component, ProbeReport, ProbeResult, ProbeStatus};

/// The user's choice after viewing the modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealActionChoice {
    /// Repair the listed components.
    Repair(Vec<Component>),
    /// Continue without repairing (rail nudge later).
    Skip,
    /// Quit Anvil.
    Quit,
}

/// Show the modal and return the user's choice.
///
/// `input` is an injectable line-reader (the live caller passes
/// `std::io::stdin().lock()`); tests pass a `&[u8]` cursor.  `output`
/// receives the rendered modal text.  Both interfaces are spelled out
/// explicitly so the modal is fully testable without a TTY.
pub fn show_healing_modal<R: BufRead, W: Write>(
    report: &ProbeReport,
    mut input: R,
    mut output: W,
) -> io::Result<HealActionChoice> {
    let rendered = render_modal(report);
    output.write_all(rendered.as_bytes())?;
    output.flush()?;

    // Map index → component for the repairable subset.
    let repairables: Vec<Component> = report
        .probes
        .iter()
        .filter(|p| matches!(p.status, ProbeStatus::Drift(_) | ProbeStatus::Broken(_)))
        .map(|p| p.component)
        .collect();

    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        // EOF — treat as "continue".
        return Ok(HealActionChoice::Skip);
    }
    let response = line.trim().to_lowercase();

    Ok(parse_response(&response, &repairables))
}

/// Live entry point used by main.rs.  Connects the modal to the real
/// stdin/stdout.  Returns `Skip` immediately if stdin isn't a TTY (e.g.
/// running under a pipe, or in CI where Anvil shouldn't auto-prompt).
pub fn show_healing_modal_live(report: &ProbeReport) -> io::Result<HealActionChoice> {
    if !io::stdin().is_terminal() {
        return Ok(HealActionChoice::Skip);
    }
    let stdin = io::stdin();
    let lock = stdin.lock();
    let stdout = io::stdout();
    show_healing_modal(report, lock, stdout.lock())
}

/// Render the modal frame to a String.
fn render_modal(report: &ProbeReport) -> String {
    let mut out = String::new();
    out.push('\n');
    out.push_str("\x1b[1;33m┌─ Anvil setup needs attention ─────────────────────────────┐\x1b[0m\n");
    out.push_str("\x1b[33m│\x1b[0m                                                            \x1b[33m│\x1b[0m\n");
    out.push_str(
        "\x1b[33m│\x1b[0m  We checked your install and found:                       \x1b[33m│\x1b[0m\n",
    );
    out.push_str("\x1b[33m│\x1b[0m                                                            \x1b[33m│\x1b[0m\n");

    let mut repair_idx = 1usize;
    for probe in &report.probes {
        if matches!(probe.status, ProbeStatus::Healthy | ProbeStatus::NotApplicable(_)) {
            continue;
        }
        let line = render_probe_line(probe, repair_idx);
        out.push_str(&line);
        repair_idx += 1;
    }

    out.push_str("\x1b[33m│\x1b[0m                                                            \x1b[33m│\x1b[0m\n");
    out.push_str(
        "\x1b[33m│\x1b[0m  [a] Repair all   [r N …] Repair listed   [c] Continue   [q] Quit \x1b[33m│\x1b[0m\n",
    );
    out.push_str("\x1b[1;33m└────────────────────────────────────────────────────────────┘\x1b[0m\n");
    out.push_str("> ");
    out
}

fn render_probe_line(probe: &ProbeResult, idx: usize) -> String {
    let (icon, color) = match probe.status {
        ProbeStatus::Broken(_) => ("✗", "\x1b[31m"),
        ProbeStatus::Drift(_) => ("⚠", "\x1b[33m"),
        _ => (" ", "\x1b[0m"),
    };
    let reason = probe.status.reason();
    let suffix = if probe.repair_fn.is_some() {
        format!("[{idx}] repair")
    } else {
        "(no auto-repair)".to_string()
    };
    format!(
        "\x1b[33m│\x1b[0m    {color}{icon}\x1b[0m {} {} — {} \x1b[33m│\x1b[0m\n",
        probe.component.label(),
        format!("(elapsed {}ms)", probe.elapsed_ms),
        format!("{reason}  {suffix}")
    )
}

/// Pure parser for the response line — heavily unit-tested.
pub fn parse_response(response: &str, repairables: &[Component]) -> HealActionChoice {
    let response = response.trim();
    if response.is_empty() || response == "c" || response == "continue" {
        return HealActionChoice::Skip;
    }
    if response == "q" || response == "quit" || response == "exit" {
        return HealActionChoice::Quit;
    }
    if response == "a" || response == "all" {
        return HealActionChoice::Repair(repairables.to_vec());
    }
    if let Some(rest) = response.strip_prefix('r').map(str::trim_start) {
        let mut chosen: Vec<Component> = Vec::new();
        for token in rest.split(|c: char| c.is_whitespace() || c == ',') {
            if token.is_empty() {
                continue;
            }
            if let Ok(n) = token.parse::<usize>() {
                // 1-based, like the modal renders.
                if n >= 1 && n <= repairables.len() {
                    let comp = repairables[n - 1];
                    if !chosen.contains(&comp) {
                        chosen.push(comp);
                    }
                }
            }
        }
        if !chosen.is_empty() {
            return HealActionChoice::Repair(chosen);
        }
    }
    // Fallback: anything we don't recognize ≡ Skip (safer than auto-repairing).
    HealActionChoice::Skip
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::report::{ProbeResult, ProbeStatus};
    use std::io::Cursor;
    use std::sync::Arc;

    fn fake_report() -> ProbeReport {
        let r: super::super::report::RepairFn = Arc::new(|| Ok("repaired".to_string()));
        ProbeReport::new(
            vec![
                ProbeResult::healthy(Component::Config, 1),
                ProbeResult::drift(Component::Qmd, "stale".into(), 2)
                    .with_repair(Arc::clone(&r)),
                ProbeResult::broken(Component::Ollama, "down".into(), 3).with_repair(r),
            ],
            6,
        )
    }

    #[test]
    fn parse_response_empty_means_skip() {
        let repairables = vec![Component::Ollama, Component::Qmd];
        assert_eq!(parse_response("", &repairables), HealActionChoice::Skip);
        assert_eq!(parse_response("c", &repairables), HealActionChoice::Skip);
        assert_eq!(
            parse_response("continue", &repairables),
            HealActionChoice::Skip
        );
    }

    #[test]
    fn parse_response_q_means_quit() {
        let repairables: Vec<Component> = vec![];
        assert_eq!(parse_response("q", &repairables), HealActionChoice::Quit);
        assert_eq!(
            parse_response("quit", &repairables),
            HealActionChoice::Quit
        );
        assert_eq!(
            parse_response("exit", &repairables),
            HealActionChoice::Quit
        );
    }

    #[test]
    fn parse_response_a_means_all() {
        let repairables = vec![Component::Vault, Component::Qmd, Component::Ollama];
        let choice = parse_response("a", &repairables);
        assert_eq!(choice, HealActionChoice::Repair(repairables.clone()));
        assert_eq!(parse_response("all", &repairables), HealActionChoice::Repair(repairables));
    }

    #[test]
    fn parse_response_r_with_indices() {
        let repairables = vec![Component::Vault, Component::Qmd, Component::Ollama];
        match parse_response("r 1 3", &repairables) {
            HealActionChoice::Repair(picks) => {
                assert_eq!(picks, vec![Component::Vault, Component::Ollama]);
            }
            other => panic!("expected Repair, got {:?}", other),
        }
    }

    #[test]
    fn parse_response_r_ignores_out_of_range() {
        let repairables = vec![Component::Vault];
        match parse_response("r 1 5 99", &repairables) {
            HealActionChoice::Repair(picks) => assert_eq!(picks, vec![Component::Vault]),
            other => panic!("expected Repair, got {:?}", other),
        }
    }

    #[test]
    fn parse_response_r_with_no_valid_indices_skips() {
        let repairables = vec![Component::Vault];
        assert_eq!(
            parse_response("r 99 100", &repairables),
            HealActionChoice::Skip
        );
    }

    #[test]
    fn parse_response_unknown_input_is_safe_skip() {
        let repairables = vec![Component::Vault];
        // Unknown input ≡ Skip.  Never accidentally trigger repair.
        assert_eq!(
            parse_response("yes please", &repairables),
            HealActionChoice::Skip
        );
        assert_eq!(parse_response("repair", &repairables), HealActionChoice::Skip);
    }

    #[test]
    fn render_modal_lists_only_drifted_and_broken_components() {
        let report = fake_report();
        let s = render_modal(&report);
        // Config is healthy → not rendered.
        assert!(!s.contains("Config"));
        // Drifted + broken get rendered.
        assert!(s.contains("QMD"));
        assert!(s.contains("Ollama"));
        // Index markers (1-based).
        assert!(s.contains("[1]"));
        assert!(s.contains("[2]"));
    }

    #[test]
    fn show_modal_full_round_trip_continue() {
        let report = fake_report();
        let input = Cursor::new(b"\n");
        let mut output: Vec<u8> = Vec::new();
        let choice = show_healing_modal(&report, input, &mut output).unwrap();
        assert_eq!(choice, HealActionChoice::Skip);
        let rendered = String::from_utf8(output).unwrap();
        assert!(rendered.contains("Anvil setup needs attention"));
    }

    #[test]
    fn show_modal_full_round_trip_repair_all() {
        let report = fake_report();
        let input = Cursor::new(b"a\n");
        let mut output: Vec<u8> = Vec::new();
        let choice = show_healing_modal(&report, input, &mut output).unwrap();
        match choice {
            HealActionChoice::Repair(comps) => {
                assert_eq!(comps, vec![Component::Qmd, Component::Ollama]);
            }
            other => panic!("expected Repair, got {:?}", other),
        }
    }

    #[test]
    fn show_modal_full_round_trip_repair_selected_index() {
        let report = fake_report();
        let input = Cursor::new(b"r 2\n");
        let mut output: Vec<u8> = Vec::new();
        let choice = show_healing_modal(&report, input, &mut output).unwrap();
        match choice {
            HealActionChoice::Repair(comps) => assert_eq!(comps, vec![Component::Ollama]),
            other => panic!("expected Repair, got {:?}", other),
        }
    }
}
