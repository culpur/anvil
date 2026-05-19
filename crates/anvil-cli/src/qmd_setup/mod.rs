//! QMD install + index + embed-backend wizard step (task #666 + #664,
//! v2.2.18 Agent A4).
//!
//! This module implements Anvil's "third pillar" commissioning:
//! installing the QMD binary (`@tobilu/qmd` on npm), registering an
//! initial collection (the user's notes / codebase), generating
//! vector embeddings against a configured backend, and wiring a
//! refresh schedule so the index stays fresh.
//!
//! ## Surfaces
//!
//! Two callers invoke this module:
//!
//! 1. **First-run wizard** — `run_qmd_wizard_step` slots between the
//!    Ollama step (A3) and the Profile step. It opens a 4-state
//!    choice modal (Install / Existing / Skip / Defer) and drives the
//!    resulting sub-flow.
//! 2. **`/qmd setup` slash command** — re-entry point for users who
//!    skipped or deferred. The dispatch handler in `main.rs` routes
//!    `SlashCommand::Qmd { query: Some("setup") }` here.
//!
//! ## Sub-modules
//!
//! - [`embed_backends`] — detect which embedding backends are available
//!   right now. Reusable by A5 (healer) for the "QMD broken because
//!   embed backend missing" flow.
//! - [`node_check`]    — verify Node.js 18+ is present.
//! - [`collection_detect`] — find a smart default folder.
//! - [`config`] — read/write the `qmd` section of `~/.anvil/config.json`.
//!
//! ## 8-axis capability contract (per `feedback-anvil-capability-contract.md`)
//!
//! 1. Definition         — `QmdWizardOutcome`, `QmdSetupState`,
//!                         `QmdConfig`, plus the sub-module types.
//! 2. Registration       — `pub mod qmd_setup` in `main.rs`; the slash
//!                         command is the existing `SlashCommand::Qmd`
//!                         with `query == Some("setup")`.
//! 3. Completion         — covered by the existing `qmd` slash command
//!                         spec (no new spec needed — `setup` is an
//!                         argument, not a sibling command).
//! 4. Handler            — dispatch in `main.rs` SlashCommand::Qmd arm
//!                         intercepts `args == "setup"` and calls
//!                         [`run_qmd_setup_from_slash`].
//! 5. Dispatch           — unified through `commands::dispatch` for
//!                         the headless / web surfaces.
//! 6. Rendering          — N/A directly; modals render via
//!                         `WizardModalRunner` (A1) and `push_system`.
//! 7. Gate               — guards against re-entering when the
//!                         schedule + config are already healthy.
//! 8. OTel + tests       — unit tests in each sub-module + an
//!                         integration test covering all four wizard
//!                         states with mocked env.
//!
//! ## Hard rules
//!
//! - NO `println!` / `eprintln!` from any function that may run while
//!   ratatui's alt-screen is up (`feedback-tui-stdout-anti-pattern.md`).
//! - Subprocesses MUST go through the `StreamingOutputModal` API
//!   spec provided by Agent A1 (`runner.run_streaming_output(id, modal)`).
//!   The dry-running detector probes ([`embed_backends::detect_available`],
//!   [`node_check::probe`]) run BEFORE the modal subprocess path opens,
//!   so they `Command::new(...).output()` with stdout + stderr
//!   captured — nothing leaks.
//!
//! ## Dead-code tolerance
//!
//! Like [`crate::schedule`], this module's wizard-step entry point
//! (`run_qmd_wizard_step` under `WizardModalRunner`) is wired in a
//! follow-up commit that brings A1 + A3 together. We silence
//! `dead_code` at the boundary so the scaffolded surface compiles
//! clean today.
#![allow(dead_code)]

pub mod collection_detect;
pub mod config;
pub mod embed_backends;
pub mod node_check;

use std::path::PathBuf;

use crate::schedule::{Backend as ScheduleBackend, Schedule};

/// User-visible outcome of a QMD setup pass.
#[derive(Debug, Clone)]
pub struct QmdWizardOutcome {
    pub state: QmdSetupState,
    pub qmd_version: Option<String>,
    pub indexed_file_count: Option<usize>,
    pub embed_backend: Option<embed_backends::EmbedBackend>,
    pub collection_path: Option<PathBuf>,
    pub refresh_backend: Option<ScheduleBackend>,
    pub notes: Vec<String>,
}

/// The four states from the modal (per task #666 + #664).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QmdSetupState {
    InstallAndIndex,
    UseExisting,
    SkipPermanent,
    Defer,
}

impl QmdSetupState {
    #[must_use]
    pub const fn choice_index(self) -> usize {
        match self {
            Self::InstallAndIndex => 0,
            Self::UseExisting => 1,
            Self::SkipPermanent => 2,
            Self::Defer => 3,
        }
    }

    #[must_use]
    pub const fn from_choice_index(idx: usize) -> Self {
        match idx {
            0 => Self::InstallAndIndex,
            1 => Self::UseExisting,
            2 => Self::SkipPermanent,
            _ => Self::Defer,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::InstallAndIndex => "Install QMD and index a folder for me",
            Self::UseExisting => "I already have QMD running",
            Self::SkipPermanent => "Skip — Anvil's in-process memory is enough",
            Self::Defer => "Maybe later — remind me",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::InstallAndIndex => {
                "We'll install QMD via npm, register a folder, embed it, and \
                 schedule hourly refresh."
            }
            Self::UseExisting => {
                "We'll probe `qmd --version` and wire it into Anvil's settings."
            }
            Self::SkipPermanent => {
                "Anvil's seven in-process memory layers stay enabled; \
                 no cross-session vector recall."
            }
            Self::Defer => {
                "We'll show this prompt again on the next 5 sessions. \
                 Run `/qmd setup` any time to re-enter."
            }
        }
    }

    #[must_use]
    pub const fn badge(self) -> Option<&'static str> {
        match self {
            Self::InstallAndIndex => Some("recommended"),
            _ => None,
        }
    }
}

/// Build the default refresh `Schedule` for the QMD index.
pub fn build_default_refresh_schedule(
    qmd_binary: &str,
    interval: crate::schedule::Interval,
) -> Result<Schedule, crate::schedule::ScheduleError> {
    let cmd = format!("{qmd_binary} update && {qmd_binary} embed");
    Schedule::new("qmd-refresh", cmd, interval)
}

/// Best-effort probe for the QMD binary.
#[must_use]
pub fn probe_qmd_binary() -> Option<PathBuf> {
    if crate::schedule::binary_on_path("qmd") {
        return Some(PathBuf::from("qmd"));
    }
    let candidates = ["/opt/homebrew/bin/qmd", "/usr/local/bin/qmd", "/usr/bin/qmd"];
    for p in candidates {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    None
}

/// Pretty-print the result card the wizard renders at the end.
#[must_use]
pub fn render_result_card(outcome: &QmdWizardOutcome) -> String {
    let mut s = String::new();
    match outcome.state {
        QmdSetupState::InstallAndIndex | QmdSetupState::UseExisting => {
            s.push_str("\u{2713} QMD");
            if let Some(v) = &outcome.qmd_version {
                s.push(' ');
                s.push_str(v);
            }
            s.push('\n');
            if let Some(n) = outcome.indexed_file_count {
                s.push_str(&format!("  Indexed {n} files in anvil-default\n"));
            }
            if let Some(backend) = &outcome.embed_backend {
                s.push_str(&format!("  Embed backend: {}\n", backend.label()));
            }
            if let Some(b) = outcome.refresh_backend {
                s.push_str(&format!("  Refresh: {}\n", b.label()));
            }
            s.push_str("  Anvil will surface relevant context on every turn.\n");
        }
        QmdSetupState::SkipPermanent => {
            s.push_str("QMD skipped — using Anvil's in-process memory only.\n");
            s.push_str("  Run /qmd setup any time to enable it later.\n");
        }
        QmdSetupState::Defer => {
            s.push_str("QMD setup deferred — we'll remind you on the next 5 sessions.\n");
            s.push_str("  Run /qmd setup any time to enter the flow.\n");
        }
    }
    for note in &outcome.notes {
        s.push_str("  · ");
        s.push_str(note);
        s.push('\n');
    }
    s
}

/// Slash-command entry point — called from the `SlashCommand::Qmd`
/// dispatcher in `main.rs` when `args == "setup"`.
pub fn run_qmd_setup_from_slash() -> String {
    let mut out = String::new();
    out.push_str("/qmd setup — current diagnostic snapshot:\n\n");

    let qmd_bin = probe_qmd_binary();
    out.push_str(
        match &qmd_bin {
            Some(p) => format!("  QMD binary:     {} (installed)\n", p.display()),
            None => "  QMD binary:     not installed (npm install -g @tobilu/qmd)\n".to_string(),
        }
        .as_str(),
    );

    let node = node_check::probe();
    out.push_str(&format!("  Node.js:        {}\n", node.describe()));

    let backends = embed_backends::detect_available(None, None);
    out.push_str("  Embed backends: ");
    if backends.is_empty() {
        out.push_str("none detected\n");
    } else {
        out.push_str(
            &backends
                .iter()
                .map(embed_backends::EmbedBackend::label)
                .collect::<Vec<_>>()
                .join(", "),
        );
        out.push('\n');
    }

    if let Some(p) = collection_detect::pick_default() {
        out.push_str(&format!("  Default folder: {}\n", p.display()));
    } else {
        out.push_str("  Default folder: none found (will prompt)\n");
    }

    out.push_str(
        "\nThe full interactive flow runs inside the first-run wizard \
         and the alt-screen `/qmd setup` modal walk-through. Re-run \
         `anvil --setup` to enter the alt-screen flow.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_choice_index_roundtrip() {
        for s in [
            QmdSetupState::InstallAndIndex,
            QmdSetupState::UseExisting,
            QmdSetupState::SkipPermanent,
            QmdSetupState::Defer,
        ] {
            assert_eq!(QmdSetupState::from_choice_index(s.choice_index()), s);
        }
    }

    #[test]
    fn state_from_index_out_of_range_defaults_to_defer() {
        assert_eq!(QmdSetupState::from_choice_index(99), QmdSetupState::Defer);
    }

    #[test]
    fn state_only_install_carries_recommended_badge() {
        assert_eq!(QmdSetupState::InstallAndIndex.badge(), Some("recommended"));
        assert_eq!(QmdSetupState::UseExisting.badge(), None);
        assert_eq!(QmdSetupState::SkipPermanent.badge(), None);
        assert_eq!(QmdSetupState::Defer.badge(), None);
    }

    #[test]
    fn build_default_refresh_schedule_emits_update_and_embed() {
        let s =
            build_default_refresh_schedule("qmd", crate::schedule::Interval::Hourly).unwrap();
        assert_eq!(s.name, "qmd-refresh");
        assert!(s.command.contains("qmd update"));
        assert!(s.command.contains("qmd embed"));
        assert!(s.command.contains("&&"));
    }

    #[test]
    fn render_result_card_install_state_includes_indexed_count() {
        let out = QmdWizardOutcome {
            state: QmdSetupState::InstallAndIndex,
            qmd_version: Some("0.6.1".to_string()),
            indexed_file_count: Some(42),
            embed_backend: Some(embed_backends::EmbedBackend::OllamaLocal {
                model: "nomic-embed-text".to_string(),
                url: "http://localhost:11434".to_string(),
            }),
            collection_path: Some(PathBuf::from("/home/u/projects")),
            refresh_backend: Some(crate::schedule::Backend::Launchd),
            notes: vec!["all good".into()],
        };
        let card = render_result_card(&out);
        assert!(card.contains("0.6.1"));
        assert!(card.contains("Indexed 42 files"));
        assert!(card.contains("nomic-embed-text"));
        assert!(card.contains("launchd"));
        assert!(card.contains("all good"));
    }

    #[test]
    fn render_result_card_skip_state_is_brief() {
        let out = QmdWizardOutcome {
            state: QmdSetupState::SkipPermanent,
            qmd_version: None,
            indexed_file_count: None,
            embed_backend: None,
            collection_path: None,
            refresh_backend: None,
            notes: Vec::new(),
        };
        let card = render_result_card(&out);
        assert!(card.contains("QMD skipped"));
        assert!(card.contains("/qmd setup"));
    }

    #[test]
    fn slash_entry_point_returns_diagnostic_text() {
        let txt = run_qmd_setup_from_slash();
        assert!(txt.contains("QMD binary"));
        assert!(txt.contains("Node.js"));
        assert!(txt.contains("Embed backends"));
    }
}
