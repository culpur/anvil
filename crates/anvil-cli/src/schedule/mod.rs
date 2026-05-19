//! Cross-platform recurring schedule installer (task #666, Agent A4).
//!
//! Anvil v2.2.18 needs to keep the QMD index fresh by running
//! `qmd update && qmd embed` on a periodic schedule. The native
//! mechanism differs per OS:
//!
//! - macOS                       → `launchd` (LaunchAgent plist in
//!                                 `~/Library/LaunchAgents/`)
//! - Linux with `systemctl --user`→ systemd-user `.service` + `.timer`
//! - Linux/BSD without systemd   → `cron` entry via `crontab -l | crontab -`
//!
//! This module exposes a single [`Schedule`] struct + three backend
//! implementations that all share the same install / uninstall / status
//! API. The backend is selected automatically by [`Schedule::backend`]
//! based on the running OS + available binaries; callers can also pick
//! a backend explicitly via [`Schedule::with_backend`] when they need
//! deterministic behavior (e.g. tests).
//!
//! The module is **reusable** — A4 uses it for the QMD refresh
//! schedule, A5 (healer) uses [`Schedule::status`] to detect whether a
//! previously-installed schedule still exists, and the v2.2.18 routines
//! daemon (#657) will use it to schedule user-defined routines.
//!
//! ## 8-axis capability contract (per `feedback-anvil-capability-contract.md`)
//!
//! 1. Definition       — [`Schedule`], [`Interval`], [`Backend`],
//!                       [`InstalledSchedule`], [`ScheduleStatus`],
//!                       [`ScheduleError`].
//! 2. Registration     — `pub mod schedule` in `main.rs`; backends are
//!                       sub-modules `launchd` / `systemd_user` / `cron`.
//! 3. Completion       — N/A (library helper, not a slash command).
//! 4. Handler          — `Schedule::{install,uninstall,status}` route
//!                       to the chosen backend's implementation.
//! 5. Dispatch         — call sites: QMD wizard step, `/qmd setup`,
//!                       A5 healer, v2.2.18 routines daemon.
//! 6. Rendering        — N/A (no TUI surface).
//! 7. Gate             — auto-detect backend; reject empty name /
//!                       empty command at install time.
//! 8. OTel + tests     — unit tests at the bottom of this file +
//!                       per-backend module tests.
//!
//! ## Hard rule on subprocess output
//!
//! No `println!` / `eprintln!` while ratatui's alt-screen is up
//! (`feedback-tui-stdout-anti-pattern.md`). Backends RETURN any
//! diagnostic strings via the [`InstalledSchedule::notes`] field.
//!
//! ## NetBSD/FreeBSD cron note
//!
//! BSD cron does NOT need a separate `crond -r` reload after the
//! crontab is replaced — the daemon polls the spool every minute.
//!
//! ## Dead-code tolerance
//!
//! The schedule module ships its full public surface ahead of the
//! wizard adapter that calls `Schedule::install` (task #666 follow-up
//! commit that ties A1's `WizardModalRunner` + A3's Ollama state). We
//! silence `dead_code` at the module boundary so the scaffolded API
//! (Interval variants, Backend variants, InstalledSchedule fields)
//! compiles clean ahead of the wiring commit.
#![allow(dead_code)]

pub mod cron;
pub mod launchd;
pub mod systemd_user;

use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Interval {
    Every15Min,
    Hourly,
    Every4Hours,
    Daily { hour: u8 },
    Custom(String),
}

impl Interval {
    #[must_use]
    pub fn to_cron_expr(&self) -> String {
        match self {
            Self::Every15Min => "*/15 * * * *".to_string(),
            Self::Hourly => "0 * * * *".to_string(),
            Self::Every4Hours => "0 */4 * * *".to_string(),
            Self::Daily { hour } => format!("0 {hour} * * *"),
            Self::Custom(expr) => expr.clone(),
        }
    }

    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Every15Min => "every 15 minutes".to_string(),
            Self::Hourly => "every hour".to_string(),
            Self::Every4Hours => "every 4 hours".to_string(),
            Self::Daily { hour } => format!("daily at {hour:02}:00"),
            Self::Custom(expr) => format!("custom ({expr})"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Auto,
    Launchd,
    SystemdUser,
    Cron,
}

impl Backend {
    #[must_use]
    pub fn resolve(self) -> Self {
        if !matches!(self, Self::Auto) {
            return self;
        }
        if cfg!(target_os = "macos") {
            return Self::Launchd;
        }
        if cfg!(target_os = "linux") && which_systemctl_user_available() {
            return Self::SystemdUser;
        }
        Self::Cron
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Launchd => "launchd",
            Self::SystemdUser => "systemd-user",
            Self::Cron => "cron",
        }
    }
}

fn which_systemctl_user_available() -> bool {
    binary_on_path("systemctl")
}

pub(crate) fn binary_on_path(name: &str) -> bool {
    let Some(path_env) = std::env::var_os("PATH") else {
        return false;
    };
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.BAT;.CMD".to_string())
            .split(';')
            .map(str::to_string)
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path_env) {
        for ext in &exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return true;
            }
        }
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub name: String,
    pub command: String,
    pub interval: Interval,
    pub backend: Backend,
}

impl Schedule {
    pub fn new(
        name: impl Into<String>,
        command: impl Into<String>,
        interval: Interval,
    ) -> Result<Self, ScheduleError> {
        let name = name.into();
        let command = command.into();
        if name.trim().is_empty() {
            return Err(ScheduleError::InvalidInput(
                "schedule name must not be empty".to_string(),
            ));
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
        {
            return Err(ScheduleError::InvalidInput(format!(
                "schedule name must be alphanumeric plus `-` `.` `_`; got `{name}`"
            )));
        }
        if command.trim().is_empty() {
            return Err(ScheduleError::InvalidInput(
                "schedule command must not be empty".to_string(),
            ));
        }
        Ok(Self {
            name,
            command,
            interval,
            backend: Backend::Auto,
        })
    }

    #[must_use]
    pub fn with_backend(mut self, backend: Backend) -> Self {
        self.backend = backend;
        self
    }

    #[must_use]
    pub fn resolved_backend(&self) -> Backend {
        self.backend.resolve()
    }

    pub fn install(&self) -> Result<InstalledSchedule, ScheduleError> {
        match self.resolved_backend() {
            Backend::Launchd => launchd::install(self),
            Backend::SystemdUser => systemd_user::install(self),
            Backend::Cron => cron::install(self),
            Backend::Auto => unreachable!("Backend::Auto is resolved above"),
        }
    }

    pub fn uninstall(&self) -> Result<(), ScheduleError> {
        match self.resolved_backend() {
            Backend::Launchd => launchd::uninstall(self),
            Backend::SystemdUser => systemd_user::uninstall(self),
            Backend::Cron => cron::uninstall(self),
            Backend::Auto => unreachable!("Backend::Auto is resolved above"),
        }
    }

    pub fn status(&self) -> Result<ScheduleStatus, ScheduleError> {
        match self.resolved_backend() {
            Backend::Launchd => launchd::status(self),
            Backend::SystemdUser => systemd_user::status(self),
            Backend::Cron => cron::status(self),
            Backend::Auto => unreachable!("Backend::Auto is resolved above"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstalledSchedule {
    pub backend: Backend,
    pub artifacts: Vec<PathBuf>,
    pub reload_cmd: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleStatus {
    Installed,
    InstalledButDisabled,
    NotInstalled,
}

#[derive(Debug)]
pub enum ScheduleError {
    InvalidInput(String),
    Io(String),
    Subprocess(String),
    BackendUnavailable(String),
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(m) => write!(f, "schedule: invalid input: {m}"),
            Self::Io(m) => write!(f, "schedule: io error: {m}"),
            Self::Subprocess(m) => write!(f, "schedule: subprocess error: {m}"),
            Self::BackendUnavailable(m) => write!(f, "schedule: backend unavailable: {m}"),
        }
    }
}

impl std::error::Error for ScheduleError {}

pub(crate) fn home_dir_for_test() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_to_cron_expr_renders_all_variants() {
        assert_eq!(Interval::Every15Min.to_cron_expr(), "*/15 * * * *");
        assert_eq!(Interval::Hourly.to_cron_expr(), "0 * * * *");
        assert_eq!(Interval::Every4Hours.to_cron_expr(), "0 */4 * * *");
        assert_eq!(Interval::Daily { hour: 3 }.to_cron_expr(), "0 3 * * *");
        assert_eq!(
            Interval::Custom("5 4 * * sun".to_string()).to_cron_expr(),
            "5 4 * * sun"
        );
    }

    #[test]
    fn interval_label_is_human_readable() {
        assert_eq!(Interval::Hourly.label(), "every hour");
        assert_eq!(Interval::Daily { hour: 3 }.label(), "daily at 03:00");
    }

    #[test]
    fn schedule_new_rejects_empty_name() {
        let err = Schedule::new("", "echo hi", Interval::Hourly).unwrap_err();
        assert!(matches!(err, ScheduleError::InvalidInput(_)));
    }

    #[test]
    fn schedule_new_rejects_empty_command() {
        let err = Schedule::new("qmd-refresh", "   ", Interval::Hourly).unwrap_err();
        assert!(matches!(err, ScheduleError::InvalidInput(_)));
    }

    #[test]
    fn schedule_new_rejects_unsafe_name_characters() {
        for bad in &["with space", "with/slash", "with$dollar", "name;rm"] {
            let err = Schedule::new(*bad, "echo hi", Interval::Hourly).unwrap_err();
            assert!(matches!(err, ScheduleError::InvalidInput(_)));
        }
    }

    #[test]
    fn schedule_new_accepts_alphanumeric_dot_hyphen_underscore() {
        for ok in &["qmd-refresh", "net.qmd.refresh", "a_b", "q1", "Q.1-x"] {
            assert!(Schedule::new(*ok, "echo hi", Interval::Hourly).is_ok());
        }
    }

    #[test]
    fn backend_auto_resolves_to_a_concrete_backend() {
        let resolved = Backend::Auto.resolve();
        assert!(!matches!(resolved, Backend::Auto));
    }

    #[test]
    fn backend_explicit_resolves_to_itself() {
        assert_eq!(Backend::Launchd.resolve(), Backend::Launchd);
        assert_eq!(Backend::Cron.resolve(), Backend::Cron);
        assert_eq!(Backend::SystemdUser.resolve(), Backend::SystemdUser);
    }

    #[test]
    fn schedule_error_displays_a_descriptive_message() {
        let e = ScheduleError::InvalidInput("test".to_string());
        let s = format!("{e}");
        assert!(s.contains("invalid input"));
        assert!(s.contains("test"));
    }
}
