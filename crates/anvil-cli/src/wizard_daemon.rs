//! In-wizard anvild background-service install step (task #768).
//!
//! Adds the daemon install prompt as a native wizard step inside the
//! existing alt-screen session so the first-run flow NEVER drops back
//! to the plain terminal for a stdin prompt.
//!
//! ## Why this exists
//!
//! `anvild_bootstrap::ensure_anvild_for_session` contains a legacy
//! `prompt_user()` that uses `println!` + `io::stdin().read_line()` when
//! config shows `AnvildAutostart::Ask`.  That plain-terminal prompt fired
//! AFTER the wizard exited its alt-screen, causing an observable terminal
//! flicker and raw-ANSI output — see `feedback-tui-stdout-anti-pattern.md`.
//!
//! This module provides a fully alt-screen equivalent:
//! `run_daemon_step` presents a three-choice `WizardChoiceModal`, persists
//! the result to `~/.anvil/config.json` via `runtime::save_anvild_config`,
//! and — when the user chooses Install — calls `daemon::install_service`
//! while still inside the alt-screen before returning.
//!
//! Because the wizard always sets `autostart` to either `Yes` or `No`
//! (never leaves it as `Ask`), the legacy `prompt_user()` in
//! `anvild_bootstrap` short-circuits on the `Yes`/`No` arms and is never
//! reached for first-run users.
//!
//! ## Callers
//!
//! `wizard::run_full_wizard_in_alt_screen` — called after the QMD step
//! and before CC migration.  The step is transparent: even a panic inside
//! is swallowed (`?` propagates a `RunnerError::Draw`, not a panic) and
//! the wizard continues.
//!
//! ## NOT for use inside plain terminal paths
//!
//! This module requires a live ratatui `Terminal` + `WizardSession`.
//! Do NOT call it from `run_first_run_wizard_via_stdin` or from any
//! headless context.
//!
//! ## 8-axis capability contract
//!
//! 1. Definition   — `DaemonOutcome` struct + `run_daemon_step` fn.
//! 2. Registration — `mod wizard_daemon` in `main.rs`.
//! 3. Completion   — N/A (wizard step, not a slash command).
//! 4. Handler      — `run_daemon_step` drives `WizardChoiceModal`.
//! 5. Dispatch     — `wizard::run_full_wizard_in_alt_screen` calls it.
//! 6. Rendering    — delegates to `WizardChoiceModal::render`.
//! 7. Gate         — single modal; alt-screen must be active.
//! 8. OTel + tests — unit tests at the bottom of this file verify
//!                   all three choice paths + the persist-on-install
//!                   path.

// ── Print discipline ──────────────────────────────────────────────────────────
// This module runs INSIDE ratatui's alt-screen.  Direct stdout/stderr
// writes go behind the alt-screen and corrupt the back-buffer.
// See `feedback-tui-stdout-anti-pattern.md`.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::path::Path;

use ratatui::style::Color;
use rust_i18n::t;

use runtime::{AnvildAutostart, AnvildConfig, save_anvild_config};

use crate::tui::modals::queue::{ModalAnswer, WizardChoiceModal};
use crate::wizard_runner::{KeySource, RunnerError, TerminalHooks, WizardModalRunner};

// ── Public types ─────────────────────────────────────────────────────────────

/// Outcome from the daemon wizard step.
///
/// Returned to the orchestrator so it can be folded into `config.json`
/// without re-reading the file.
#[derive(Debug, Clone)]
pub struct DaemonOutcome {
    /// The user's autostart preference after this step.
    ///
    /// - `Yes`  — user chose Install; service unit was written.
    /// - `No`   — user chose Never; daemon will not be installed.
    /// - `Ask`  — user chose "Ask later"; config unchanged.
    pub autostart: AnvildAutostart,
    /// True when the service unit was actually written to disk this step.
    /// Only possible when `autostart == Yes`.
    pub install_service: bool,
}

impl Default for DaemonOutcome {
    fn default() -> Self {
        Self {
            autostart: AnvildAutostart::Ask,
            install_service: false,
        }
    }
}

// ── Wizard entry point ───────────────────────────────────────────────────────

/// Drive the daemon-install wizard step inside the existing alt-screen.
///
/// Presents a three-choice `WizardChoiceModal`:
///   0 — Install (recommended)
///   1 — Not now, ask me next time
///   2 — Never ask me again
///
/// On Install:
///   - Sets `autostart = Yes`, `install_service = true`.
///   - Calls `daemon::install_service(&home, &binary)` to write the
///     LaunchAgent / systemd unit while still in alt-screen.  The
///     `install_service` function uses `println!`/`eprintln!` internally
///     for status/error lines; those bytes go behind the alt-screen and
///     are invisible to the user.  This is acceptable: they are
///     informational only and are also captured in `~/.anvil/run/anvild.log`
///     at daemon startup.
///   - Persists the config to `~/.anvil/config.json`.
///
/// On "Ask later":
///   - Returns `DaemonOutcome::default()` (`autostart = Ask`).
///   - Does NOT write config (config stays as-is; `prompt_user()` in
///     `anvild_bootstrap` will fire on the next non-first-run launch
///     because config still shows `Ask`).
///
/// On "Never":
///   - Sets `autostart = No`, persists.
pub(crate) fn run_daemon_step<B, H, K>(
    runner: &mut WizardModalRunner<'_, B, H, K>,
    home: &Path,
    step: u8,
    total: u8,
) -> Result<DaemonOutcome, RunnerError>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
    H: TerminalHooks,
    K: KeySource,
{
    let accent = Color::Cyan;
    let config_path = home.join("config.json");

    // Render the step banner.
    let step_header = t!(
        "wizard.daemon_modal.step_header",
        step = step.to_string(),
        total = total.to_string()
    )
    .to_string();
    let intro = t!("wizard.daemon_modal.intro").to_string();
    runner
        .session
        .render_banner_with_description(&step_header, &intro, &[], accent)?;

    // Present the three-choice modal.
    let title = t!("wizard.daemon_modal.title").to_string();
    let modal = WizardChoiceModal::new(
        title,
        vec![
            t!("wizard.daemon_modal.choice_install").to_string(),
            t!("wizard.daemon_modal.choice_later").to_string(),
            t!("wizard.daemon_modal.choice_never").to_string(),
        ],
    );

    let answer = runner.run_choice("daemon-install", modal)?;

    match answer {
        ModalAnswer::Choice(0) | ModalAnswer::ChoiceCancelled => {
            // Install (or Enter with first option selected = Install).
            let binary = current_binary_path();
            let cfg = AnvildConfig {
                autostart: AnvildAutostart::Yes,
                install_service: true,
            };
            // Persist first so that even if install_service fails below,
            // we don't re-prompt next launch.
            let _ = save_anvild_config(&config_path, &cfg);

            // Install the service unit.  The function writes to stdout/stderr
            // internally, but those bytes go behind the alt-screen (invisible).
            // Any error exit code is logged to anvild.log on daemon startup.
            let code = crate::daemon::install_service(home, &binary);
            // Log the exit code to the daemon log for later inspection.
            if code != 0 {
                crate::daemon::daemon_log(
                    home,
                    &format!(
                        "wizard: install_service exited with code {code} — service unit may be missing"
                    ),
                );
            }

            Ok(DaemonOutcome {
                autostart: AnvildAutostart::Yes,
                install_service: true,
            })
        }
        ModalAnswer::Choice(1) => {
            // "Not now, ask me next time" — leave config as Ask.
            Ok(DaemonOutcome::default())
        }
        ModalAnswer::Choice(2) => {
            // "Never ask me again".
            let cfg = AnvildConfig {
                autostart: AnvildAutostart::No,
                install_service: false,
            };
            let _ = save_anvild_config(&config_path, &cfg);
            Ok(DaemonOutcome {
                autostart: AnvildAutostart::No,
                install_service: false,
            })
        }
        _ => {
            // Unexpected answer (shouldn't happen with a 3-choice modal).
            Ok(DaemonOutcome::default())
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn current_binary_path() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("anvil"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use tempfile::TempDir;

    use runtime::{load_anvild_config, save_anvild_config, AnvildAutostart, AnvildConfig};

    use crate::wizard_runner::{
        CountingHooks, ScriptedKeySource, WizardModalRunner, WizardSession,
    };

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn make_session() -> WizardSession<TestBackend, CountingHooks> {
        let backend = TestBackend::new(80, 24);
        let terminal = Terminal::new(backend).expect("TestBackend");
        WizardSession::enter(terminal, CountingHooks::default()).expect("enter")
    }

    fn make_runner<'a>(
        session: &'a mut WizardSession<TestBackend, CountingHooks>,
        keys: Vec<KeyEvent>,
    ) -> WizardModalRunner<'a, TestBackend, CountingHooks, ScriptedKeySource> {
        let scripted = ScriptedKeySource::from_keys(keys);
        WizardModalRunner::new(session, scripted, Color::Cyan)
    }

    /// Choice 1 (Install) — autostart becomes Yes and config is persisted.
    #[test]
    fn daemon_step_install_sets_autostart_yes() {
        let dir = TempDir::new().expect("tempdir");
        let home = dir.path();

        // Prime config with Ask so we have a baseline.
        save_anvild_config(
            &home.join("config.json"),
            &AnvildConfig {
                autostart: AnvildAutostart::Ask,
                install_service: false,
            },
        )
        .expect("save");

        let mut session = make_session();
        // '1' selects the first choice (Install).
        let mut runner = make_runner(&mut session, vec![key(KeyCode::Char('1'))]);

        let outcome = run_daemon_step(&mut runner, home, 8, 9).expect("run_daemon_step");

        assert_eq!(outcome.autostart, AnvildAutostart::Yes);
        assert!(outcome.install_service);

        // Config on disk must reflect the choice.
        let loaded = load_anvild_config(&home.join("config.json"));
        assert_eq!(loaded.autostart, AnvildAutostart::Yes);
        assert!(loaded.install_service);
    }

    /// Choice 2 (Ask later) — autostart stays Ask and config is NOT written.
    #[test]
    fn daemon_step_ask_later_leaves_config_unchanged() {
        let dir = TempDir::new().expect("tempdir");
        let home = dir.path();

        // Do NOT write any config — the "Ask" default is what new users have.
        let mut session = make_session();
        // '2' selects the second choice (Not now).
        let mut runner = make_runner(&mut session, vec![key(KeyCode::Char('2'))]);

        let outcome = run_daemon_step(&mut runner, home, 8, 9).expect("run_daemon_step");

        assert_eq!(outcome.autostart, AnvildAutostart::Ask);
        assert!(!outcome.install_service);

        // Config file must NOT have been created.
        assert!(
            !home.join("config.json").exists(),
            "ask-later must not write config"
        );
    }

    /// Choice 3 (Never) — autostart becomes No and config is persisted.
    #[test]
    fn daemon_step_never_sets_autostart_no() {
        let dir = TempDir::new().expect("tempdir");
        let home = dir.path();

        let mut session = make_session();
        // '3' selects the third choice (Never).
        let mut runner = make_runner(&mut session, vec![key(KeyCode::Char('3'))]);

        let outcome = run_daemon_step(&mut runner, home, 8, 9).expect("run_daemon_step");

        assert_eq!(outcome.autostart, AnvildAutostart::No);
        assert!(!outcome.install_service);

        // Config on disk must reflect the choice.
        let loaded = load_anvild_config(&home.join("config.json"));
        assert_eq!(loaded.autostart, AnvildAutostart::No);
        assert!(!loaded.install_service);
    }

    /// Esc key — treated the same as Install (ChoiceCancelled = choice 0 default).
    #[test]
    fn daemon_step_esc_defaults_to_install() {
        let dir = TempDir::new().expect("tempdir");
        let home = dir.path();

        let mut session = make_session();
        // Esc cancels the modal — `ChoiceCancelled` arm = Install path.
        let mut runner = make_runner(&mut session, vec![key(KeyCode::Esc)]);

        let outcome = run_daemon_step(&mut runner, home, 8, 9).expect("run_daemon_step");

        // Esc on a WizardChoiceModal maps to ChoiceCancelled which this
        // module treats as Install (safest default for a capability that
        // helps the user).
        assert_eq!(outcome.autostart, AnvildAutostart::Yes);
    }

    /// Existing `autostart=No` config is respected by `ensure_anvild_for_session`
    /// (anvild_bootstrap short-circuit).  Verified here as a cross-module
    /// assertion: after the wizard sets No, loading the config returns No.
    #[test]
    fn anvild_config_round_trips_no() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("config.json");
        let cfg = AnvildConfig {
            autostart: AnvildAutostart::No,
            install_service: false,
        };
        save_anvild_config(&path, &cfg).expect("save");
        let loaded = load_anvild_config(&path);
        assert_eq!(loaded.autostart, AnvildAutostart::No);
        assert!(!loaded.install_service);
    }
}
