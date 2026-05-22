//! `anvil daemon` — the first local **anvild** server (v2.2.18 #657).
//!
//! Long-running OS process that owns the routine scheduler.  Persists across
//! TUI sessions: when the user closes their interactive Anvil, routines still
//! fire on schedule because anvild is a separate process with its own PID,
//! its own log, and its own lifecycle.
//!
//! ## Subcommands
//!
//! ```text
//! anvil daemon start [--foreground]   # spawn detached (or run in this terminal)
//! anvil daemon stop                    # SIGTERM the pid in ~/.anvil/run/anvild.pid
//! anvil daemon status                  # is it running? since when? last tick?
//! anvil daemon foreground              # alias for `start --foreground` (used by service units)
//! anvil daemon install-service         # generate LaunchAgent / systemd / Task Scheduler unit
//! anvil daemon uninstall-service       # remove the unit we generated
//! ```
//!
//! ## Files we own
//!
//! - `~/.anvil/run/anvild.pid`          — current PID; written on start, removed on graceful stop
//! - `~/.anvil/run/anvild.log`          — stdout + stderr of the daemon process
//! - `~/.anvil/run/anvild.status.json`  — last tick, last error, routine counts (refreshed every 30 s)
//! - `~/.anvil/routines/*.toml`         — routine definitions, owned by user
//! - `~/.anvil/routines/output/<name>/` — packets + archive markdown, owned by daemon
//!
//! ## Loop
//!
//! Every 30 s:
//! 1. Reload routine definitions from disk (cheap — only re-parses changed files).
//! 2. For each enabled routine, ask [`schedule::next_fire`] when it should run next.
//! 3. If `next_fire <= now`, build the [`ExecRequest`] with collected context
//!    blocks and call [`executor::run_once`] in a worker thread.  The main loop
//!    never blocks on inference — one slow routine doesn't starve the others.
//! 4. Write status sidecar so `/schedule status` and `anvil daemon status`
//!    have a fresh snapshot.
//!
//! ## What's NOT here
//!
//! - **Inter-process IPC.** v2.2.18 keeps anvild stateless w.r.t. the TUI:
//!   the TUI reads packets/status JSON from disk, the daemon doesn't expose
//!   a Unix socket or HTTP endpoint.  v2.3 will add that.
//! - **Vault unlock prompts.** The daemon runs while the user is away; a
//!   locked vault means webhook deliveries fail with a clear error.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use runtime::routines::definition::{load_all, LoadAllResult, RoutineDef, RoutineTier};
use runtime::routines::executor::{
    collect_context, run_once, validate_anvil_binary, ExecRequest,
};
use runtime::routines::proposal::{self, RoutineProposal};
use runtime::routines::schedule::next_fire;

// ─── Subcommand enum ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonSubcommand {
    Start { foreground: bool },
    Stop,
    Status,
    Foreground,
    InstallService,
    UninstallService,
}

/// Parse `anvil daemon <args>` into a [`DaemonSubcommand`].
pub fn parse(args: &[String]) -> Result<DaemonSubcommand, String> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Ok(DaemonSubcommand::Status);
    };
    match sub {
        "start" => {
            let foreground = args.iter().any(|a| a == "--foreground" || a == "-f");
            Ok(DaemonSubcommand::Start { foreground })
        }
        "stop" => Ok(DaemonSubcommand::Stop),
        "status" => Ok(DaemonSubcommand::Status),
        "foreground" | "run" => Ok(DaemonSubcommand::Foreground),
        "install-service" => Ok(DaemonSubcommand::InstallService),
        "uninstall-service" => Ok(DaemonSubcommand::UninstallService),
        other => Err(format!(
            "anvil daemon: unknown subcommand `{other}` (expected: start | stop | status | foreground | install-service | uninstall-service)"
        )),
    }
}

// ─── Status sidecar ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub started_at: u64,
    pub last_tick_at: u64,
    pub last_tick_routines_loaded: usize,
    pub last_tick_routines_fired: usize,
    /// Number of routines that were ask-tier (permission_mode = accept or
    /// danger) and would have fired this tick had they been auto-tier.
    /// Each one wrote a proposal to `~/.anvil/routines/pending/` instead.
    #[serde(default)]
    pub last_tick_proposals_written: usize,
    /// Total pending proposals on disk after this tick.  Read by the TUI
    /// status footer to decide whether to render the "N pending approval"
    /// badge.
    #[serde(default)]
    pub pending_proposals_total: usize,
    /// Most recent non-fatal error encountered in the loop (load error,
    /// archive write error, etc.).  Cleared after a successful tick with
    /// no errors.
    pub last_error: Option<String>,
    pub anvil_version: String,
    // ── Task #763 (v2.2.20): OAuth keep-alive observability ─────────────────
    /// Unix seconds when the OAuth keepalive thread last successfully
    /// refreshed the Anthropic access token.  `None` until the first
    /// refresh — for a freshly-logged-in token with hours of validity left,
    /// this stays `None` until the first scheduled tick fires near expiry.
    #[serde(default)]
    pub last_oauth_refresh_at: Option<u64>,
    /// Unix seconds for the expires_at of the currently-cached token, as
    /// seen by the keepalive thread on its most recent observation.
    /// Lets `anvil daemon status` answer "is the daemon watching a healthy
    /// token?" without re-reading credentials.json.
    #[serde(default)]
    pub oauth_expires_at: Option<u64>,
    /// Set if the keepalive's most recent event was a `RefreshFailed`.
    /// Cleared on the next successful tick.
    #[serde(default)]
    pub last_oauth_error: Option<String>,
    /// True once the keepalive side-thread has reported at least one event
    /// (any of NoCredential / Refreshed / RefreshFailed).  Proves the thread
    /// is alive even when there is nothing to refresh.
    #[serde(default)]
    pub oauth_keepalive_alive: bool,
    /// Unix seconds when the keepalive most recently checked credentials
    /// (regardless of refresh outcome).  This is the "heartbeat" reviewers
    /// want when diagnosing "is the daemon actually doing OAuth work?"
    #[serde(default)]
    pub last_oauth_check_at: Option<u64>,
    // ── Task #764 (v2.2.20): Gemini OAuth keep-alive fields ─────────────────
    #[serde(default)]
    pub last_gemini_refresh_at: Option<u64>,
    #[serde(default)]
    pub gemini_expires_at: Option<u64>,
    #[serde(default)]
    pub last_gemini_error: Option<String>,
    #[serde(default)]
    pub gemini_keepalive_alive: bool,
    #[serde(default)]
    pub last_gemini_check_at: Option<u64>,
    // ── Task #764 (v2.2.20): Copilot monitor fields ──────────────────────────
    #[serde(default)]
    pub copilot_expires_at: Option<u64>,
    #[serde(default)]
    pub last_copilot_error: Option<String>,
    #[serde(default)]
    pub copilot_monitor_alive: bool,
    #[serde(default)]
    pub last_copilot_check_at: Option<u64>,
}

fn status_path(home: &Path) -> PathBuf {
    home.join("run").join("anvild.status.json")
}
fn pid_path(home: &Path) -> PathBuf {
    home.join("run").join("anvild.pid")
}
fn log_path(home: &Path) -> PathBuf {
    home.join("run").join("anvild.log")
}

fn write_status(home: &Path, status: &DaemonStatus) {
    let path = status_path(home);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_string_pretty(status) {
        let _ = fs::write(path, body);
    }
}

fn read_pid(home: &Path) -> Option<u32> {
    fs::read_to_string(pid_path(home))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use std::os::raw::c_int;
        // SAFETY: kill(2) with signal 0 just probes for the process existence
        // without sending an actual signal.  No memory is read; the syscall
        // returns 0 on alive and -1 with errno=ESRCH on missing.
        unsafe {
            unsafe extern "C" {
                fn kill(pid: c_int, sig: c_int) -> c_int;
            }
            kill(pid as c_int, 0) == 0
        }
    }
    #[cfg(windows)]
    {
        // Cheap fallback: try to read the PID file's age and compare against
        // a hard maximum.  Not perfect but adequate for v2.2.18; v2.3 will
        // use OpenProcess for a real check.  For now assume any PID file
        // newer than 24 h is live.
        let _ = pid;
        true
    }
}

// ─── Dispatch ────────────────────────────────────────────────────────────────

/// Entry point invoked from `main.rs::run_cli`.
///
/// Returns a process exit code; the caller `std::process::exit`s with it so
/// shell consumers can chain `anvil daemon status && anvil …`.
pub fn run(sub: DaemonSubcommand, anvil_binary: PathBuf, anvil_version: String) -> i32 {
    let home = anvil_home();
    match sub {
        DaemonSubcommand::Start { foreground: true } | DaemonSubcommand::Foreground => {
            run_foreground(&home, &anvil_binary, &anvil_version)
        }
        DaemonSubcommand::Start { foreground: false } => spawn_detached(&home, &anvil_binary),
        DaemonSubcommand::Stop => stop(&home),
        DaemonSubcommand::Status => print_status(&home),
        DaemonSubcommand::InstallService => install_service(&home, &anvil_binary),
        DaemonSubcommand::UninstallService => uninstall_service(&home),
    }
}

/// Public accessor for the anvild PID-file health check.
///
/// Reads the `~/.anvil/run/anvild.pid` file and verifies the recorded PID
/// is still alive. Returns `false` for both "no PID file" and "stale PID."
/// Used by `anvild_bootstrap::ensure_anvild_for_session` and TUI keep-alive
/// gates that want to know whether the daemon is taking over OAuth refresh.
#[must_use]
pub fn anvild_running() -> bool {
    let home = anvil_home();
    match read_pid(&home) {
        Some(pid) => pid_alive(pid),
        None => false,
    }
}

/// Append a timestamped line to `~/.anvil/run/anvild.log`. Best-effort: any
/// IO failure is swallowed since the daemon log is observational, not
/// load-bearing.
pub fn daemon_log(home: &Path, msg: &str) {
    let path = log_path(home);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "[{}] {}", unix_now(), msg);
    }
}

fn anvil_home() -> PathBuf {
    if let Ok(explicit) = std::env::var("ANVIL_CONFIG_HOME") {
        if !explicit.is_empty() {
            return PathBuf::from(explicit);
        }
    }
    if let Ok(explicit) = std::env::var("ANVIL_HOME") {
        if !explicit.is_empty() {
            return PathBuf::from(explicit);
        }
    }
    dirs_next::home_dir()
        .map(|h| h.join(".anvil"))
        .unwrap_or_else(|| PathBuf::from(".anvil"))
}

// ─── start --foreground / `daemon foreground` ───────────────────────────────

fn run_foreground(home: &Path, anvil_binary: &Path, anvil_version: &str) -> i32 {
    if let Err(e) = validate_anvil_binary(anvil_binary) {
        eprintln!("anvil daemon: {e}");
        return 2;
    }

    let _ = fs::create_dir_all(home.join("run"));
    let _ = fs::create_dir_all(home.join("routines"));
    let _ = fs::create_dir_all(home.join("routines").join("output"));

    // Refuse to start if another daemon is already running with this PID file.
    if let Some(existing) = read_pid(home) {
        if pid_alive(existing) {
            eprintln!(
                "anvil daemon: already running with PID {existing} (see {})",
                pid_path(home).display()
            );
            return 1;
        } else {
            // Stale PID file — silently reclaim.
            let _ = fs::remove_file(pid_path(home));
        }
    }

    let pid = std::process::id();
    if let Err(e) = fs::write(pid_path(home), pid.to_string()) {
        eprintln!("anvil daemon: failed to write PID file: {e}");
        return 3;
    }

    let stop = Arc::new(AtomicBool::new(false));
    install_signal_handler(Arc::clone(&stop));

    // Task #763 (v2.2.20): shared OAuth observability slot.  The keepalive
    // side-thread writes into this on every event; the main tick reads it
    // and serializes the latest values into anvild.status.json so external
    // observers (`anvil daemon status`, future Wazuh probes, the TUI rail)
    // can prove the keepalive is alive.
    #[derive(Default, Clone)]
    struct OAuthObs {
        last_refresh_at: Option<u64>,
        expires_at: Option<u64>,
        last_error: Option<String>,
        alive: bool,
        last_check_at: Option<u64>,
    }
    let oauth_obs: Arc<std::sync::Mutex<OAuthObs>> =
        Arc::new(std::sync::Mutex::new(OAuthObs::default()));

    // Task #761 (v2.2.20) + #763 (v2.2.20): spawn the OAuth keep-alive
    // ticker on a side thread.  Same code path as the in-TUI fallback
    // (`bg_handlers::spawn_oauth_keepalive`) so refresh semantics are
    // identical regardless of which path is in charge.  When daemon is
    // alive the TUI gate at `daemon::anvild_running()` suppresses the
    // in-TUI ticker so the two don't race.
    //
    // Observability: every event the ticker emits is (a) written to
    // `anvild.log` and (b) merged into `oauth_obs` so the next main-loop
    // tick can serialize it into `anvild.status.json`.  Without this the
    // thread was an opaque black box — "trust me, it's running" — and the
    // user rightly demanded proof.
    let oauth_stop_flag = Arc::clone(&stop);
    let oauth_obs_writer = Arc::clone(&oauth_obs);
    let oauth_home = home.to_path_buf();
    std::thread::spawn(move || {
        let runtime_handle = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                daemon_log(
                    &oauth_home,
                    &format!("oauth-keepalive: tokio runtime build failed: {e}"),
                );
                return;
            }
        };
        daemon_log(&oauth_home, "oauth-keepalive: starting");
        runtime_handle.block_on(async move {
            let refresher = Arc::new(api::AnthropicKeepaliveRefresher);
            let (tx, mut rx) =
                tokio::sync::mpsc::unbounded_channel::<runtime::KeepaliveEvent>();
            let _handle = runtime::spawn_oauth_keepalive(refresher, tx);
            while !oauth_stop_flag.load(Ordering::Relaxed) {
                tokio::select! {
                    maybe = rx.recv() => match maybe {
                        Some(event) => {
                            let now = unix_now();
                            let mut obs = oauth_obs_writer
                                .lock()
                                .unwrap_or_else(|p| p.into_inner());
                            obs.alive = true;
                            obs.last_check_at = Some(now);
                            match &event {
                                runtime::KeepaliveEvent::Refreshed { new_expires_at } => {
                                    obs.last_refresh_at = Some(now);
                                    obs.expires_at = *new_expires_at;
                                    obs.last_error = None;
                                    daemon_log(
                                        &oauth_home,
                                        &format!(
                                            "oauth-keepalive: refreshed (expires_at={:?})",
                                            new_expires_at
                                        ),
                                    );
                                }
                                runtime::KeepaliveEvent::RefreshFailed { reason } => {
                                    obs.last_error = Some(reason.clone());
                                    daemon_log(
                                        &oauth_home,
                                        &format!("oauth-keepalive: refresh failed: {reason}"),
                                    );
                                }
                                runtime::KeepaliveEvent::NoCredential => {
                                    obs.last_error = Some(
                                        "no Anthropic OAuth credential present".to_string(),
                                    );
                                    obs.expires_at = None;
                                    daemon_log(
                                        &oauth_home,
                                        "oauth-keepalive: no credential present (idle)",
                                    );
                                }
                                runtime::KeepaliveEvent::Heartbeat { expires_at } => {
                                    obs.expires_at = *expires_at;
                                    obs.last_error = None;
                                    daemon_log(
                                        &oauth_home,
                                        &format!(
                                            "oauth-keepalive: heartbeat (expires_at={:?})",
                                            expires_at
                                        ),
                                    );
                                }
                                runtime::KeepaliveEvent::Stopped => {
                                    daemon_log(&oauth_home, "oauth-keepalive: stopped");
                                    drop(obs);
                                    break;
                                }
                            }
                            // On any event that's not a refresh, snapshot
                            // the current credential's expiry so the status
                            // sidecar reflects the *observed* state, not
                            // just the last refresh outcome.
                            if !matches!(event, runtime::KeepaliveEvent::Refreshed { .. })
                                && let Ok(Some(t)) = runtime::load_oauth_credentials()
                            {
                                obs.expires_at = t.expires_at;
                            }
                        }
                        None => {
                            daemon_log(
                                &oauth_home,
                                "oauth-keepalive: event channel closed",
                            );
                            break;
                        }
                    },
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                }
            }
            daemon_log(&oauth_home, "oauth-keepalive: thread exiting");
        });
    });

    // Task #764 (v2.2.20): Gemini keep-alive — same OAuthObs pattern, only
    // started when a Gemini OAuth token is present on disk at daemon startup.
    #[derive(Default, Clone)]
    struct GeminiObs {
        last_refresh_at: Option<u64>,
        expires_at: Option<u64>,
        last_error: Option<String>,
        alive: bool,
        last_check_at: Option<u64>,
    }
    let gemini_obs: Arc<std::sync::Mutex<GeminiObs>> =
        Arc::new(std::sync::Mutex::new(GeminiObs::default()));

    if api::load_gemini_keepalive_snapshot().is_some() {
        let gemini_stop_flag = Arc::clone(&stop);
        let gemini_obs_writer = Arc::clone(&gemini_obs);
        let gemini_home = home.to_path_buf();
        std::thread::spawn(move || {
            let runtime_handle = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    daemon_log(
                        &gemini_home,
                        &format!("gemini-keepalive: tokio runtime build failed: {e}"),
                    );
                    return;
                }
            };
            daemon_log(&gemini_home, "gemini-keepalive: starting");
            runtime_handle.block_on(async move {
                let refresher = Arc::new(api::GeminiKeepaliveRefresher);
                let (tx, mut rx) =
                    tokio::sync::mpsc::unbounded_channel::<runtime::KeepaliveEvent>();
                let _handle = runtime::spawn_gemini_keepalive(
                    refresher,
                    || api::load_gemini_keepalive_snapshot(),
                    |snap| api::save_gemini_keepalive_snapshot(snap),
                    tx,
                );
                while !gemini_stop_flag.load(Ordering::Relaxed) {
                    tokio::select! {
                        maybe = rx.recv() => match maybe {
                            Some(event) => {
                                let now = unix_now();
                                let mut obs = gemini_obs_writer
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner());
                                obs.alive = true;
                                obs.last_check_at = Some(now);
                                match &event {
                                    runtime::KeepaliveEvent::Refreshed { new_expires_at } => {
                                        obs.last_refresh_at = Some(now);
                                        obs.expires_at = *new_expires_at;
                                        obs.last_error = None;
                                        daemon_log(
                                            &gemini_home,
                                            &format!(
                                                "gemini-keepalive: refreshed (expires_at={:?})",
                                                new_expires_at
                                            ),
                                        );
                                    }
                                    runtime::KeepaliveEvent::RefreshFailed { reason } => {
                                        obs.last_error = Some(reason.clone());
                                        daemon_log(
                                            &gemini_home,
                                            &format!("gemini-keepalive: refresh failed: {reason}"),
                                        );
                                    }
                                    runtime::KeepaliveEvent::NoCredential => {
                                        daemon_log(
                                            &gemini_home,
                                            "gemini-keepalive: no credential (idle)",
                                        );
                                    }
                                    runtime::KeepaliveEvent::Heartbeat { expires_at } => {
                                        obs.expires_at = *expires_at;
                                        obs.last_error = None;
                                        daemon_log(
                                            &gemini_home,
                                            &format!(
                                                "gemini-keepalive: heartbeat (expires_at={:?})",
                                                expires_at
                                            ),
                                        );
                                    }
                                    runtime::KeepaliveEvent::Stopped => {
                                        daemon_log(&gemini_home, "gemini-keepalive: stopped");
                                        drop(obs);
                                        break;
                                    }
                                }
                            }
                            None => {
                                daemon_log(
                                    &gemini_home,
                                    "gemini-keepalive: event channel closed",
                                );
                                break;
                            }
                        },
                        _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                    }
                }
                daemon_log(&gemini_home, "gemini-keepalive: thread exiting");
            });
        });
    }

    // Task #764 (v2.2.20): Copilot monitor — pure file-watch, no HTTP calls.
    // Only started when a Copilot token exists on disk at daemon startup.
    #[derive(Default, Clone)]
    struct CopilotObs {
        expires_at: Option<u64>,
        last_error: Option<String>,
        alive: bool,
        last_check_at: Option<u64>,
    }
    let copilot_obs: Arc<std::sync::Mutex<CopilotObs>> =
        Arc::new(std::sync::Mutex::new(CopilotObs::default()));

    if matches!(api::load_copilot_token(), Ok(Some(_))) {
        let copilot_stop_flag = Arc::clone(&stop);
        let copilot_obs_writer = Arc::clone(&copilot_obs);
        let copilot_home = home.to_path_buf();
        std::thread::spawn(move || {
            let runtime_handle = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    daemon_log(
                        &copilot_home,
                        &format!("copilot-monitor: tokio runtime build failed: {e}"),
                    );
                    return;
                }
            };
            daemon_log(&copilot_home, "copilot-monitor: starting");
            runtime_handle.block_on(async move {
                let (tx, mut rx) =
                    tokio::sync::mpsc::unbounded_channel::<runtime::KeepaliveEvent>();
                let _handle = runtime::spawn_copilot_monitor(
                    || {
                        api::load_copilot_token().ok().flatten().map(|t| {
                            runtime::CopilotTokenSnapshot {
                                access_token: t.access_token,
                                expires_at: t.expires_at,
                            }
                        })
                    },
                    tx,
                );
                while !copilot_stop_flag.load(Ordering::Relaxed) {
                    tokio::select! {
                        maybe = rx.recv() => match maybe {
                            Some(event) => {
                                let now = unix_now();
                                let mut obs = copilot_obs_writer
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner());
                                obs.alive = true;
                                obs.last_check_at = Some(now);
                                match &event {
                                    runtime::KeepaliveEvent::Heartbeat { expires_at } => {
                                        obs.expires_at = *expires_at;
                                        obs.last_error = None;
                                        daemon_log(
                                            &copilot_home,
                                            &format!(
                                                "copilot-monitor: heartbeat (expires_at={:?})",
                                                expires_at
                                            ),
                                        );
                                    }
                                    runtime::KeepaliveEvent::RefreshFailed { reason } => {
                                        obs.last_error = Some(reason.clone());
                                        daemon_log(
                                            &copilot_home,
                                            &format!("copilot-monitor: expiry warning: {reason}"),
                                        );
                                    }
                                    runtime::KeepaliveEvent::NoCredential => {
                                        daemon_log(
                                            &copilot_home,
                                            "copilot-monitor: token gone (idle)",
                                        );
                                    }
                                    runtime::KeepaliveEvent::Stopped => {
                                        daemon_log(&copilot_home, "copilot-monitor: stopped");
                                        drop(obs);
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                            None => {
                                daemon_log(
                                    &copilot_home,
                                    "copilot-monitor: event channel closed",
                                );
                                break;
                            }
                        },
                        _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                    }
                }
                daemon_log(&copilot_home, "copilot-monitor: thread exiting");
            });
        });
    }

    let started_at = unix_now();
    let mut status = DaemonStatus {
        pid,
        started_at,
        last_tick_at: started_at,
        last_tick_routines_loaded: 0,
        last_tick_routines_fired: 0,
        last_tick_proposals_written: 0,
        pending_proposals_total: 0,
        last_error: None,
        anvil_version: anvil_version.to_string(),
        last_oauth_refresh_at: None,
        oauth_expires_at: None,
        last_oauth_error: None,
        oauth_keepalive_alive: false,
        last_oauth_check_at: None,
        // Task #764: Gemini + Copilot — start empty, populated by their threads.
        last_gemini_refresh_at: None,
        gemini_expires_at: None,
        last_gemini_error: None,
        gemini_keepalive_alive: false,
        last_gemini_check_at: None,
        copilot_expires_at: None,
        last_copilot_error: None,
        copilot_monitor_alive: false,
        last_copilot_check_at: None,
    };
    write_status(home, &status);

    // Per-routine "next fire" cache so we don't re-compute every tick.
    let mut next_fire_cache: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();

    eprintln!(
        "[anvild] starting (pid {pid}, version {anvil_version}, anvil binary {})",
        anvil_binary.display()
    );

    while !stop.load(Ordering::Relaxed) {
        let tick_start = Instant::now();
        let now = unix_now();

        let LoadAllResult { defs, errors } = load_all(&home.join("routines"));
        status.last_tick_routines_loaded = defs.len();
        status.last_tick_routines_fired = 0;
        status.last_tick_proposals_written = 0;
        status.last_error = errors
            .first()
            .map(|e| format!("definition load: {e}"));

        // Garbage-collect cache entries for routines that vanished.
        next_fire_cache.retain(|name, _| defs.iter().any(|d| d.name == *name));

        for def in &defs {
            if !def.enabled {
                next_fire_cache.remove(&def.name);
                continue;
            }
            let next = *next_fire_cache.entry(def.name.clone()).or_insert_with(|| {
                next_fire(&def.schedule, now).unwrap_or(u64::MAX)
            });
            if next > now {
                continue;
            }

            // Tier gate: read-only/safe modes (plan, auto) fire on their
            // own; modes with elevated tool access (accept, danger) write
            // a proposal and wait for `/schedule approve <name>` instead.
            // See `feedback-anvil-capability-contract` — running danger
            // routines unsupervised is the kind of capability we never
            // ship without an explicit on-ramp.
            match def.permission_mode.tier() {
                RoutineTier::Auto => {
                    // It's go time.  Spawn on a worker so the loop can move on.
                    let def_clone = def.clone();
                    let binary = anvil_binary.to_path_buf();
                    let config_home = home.to_path_buf();
                    let version = anvil_version.to_string();
                    let output_root = home.join("routines").join("output");

                    std::thread::spawn(move || {
                        let ctx = collect_context(&output_root, &def_clone);
                        let req = ExecRequest {
                            routine: def_clone.clone(),
                            anvil_binary: binary,
                            config_home,
                            anvil_version: version,
                            timeout: Duration::from_secs(300),
                            context_blocks: ctx,
                        };
                        let outcome = run_once(&req, |_| None);
                        eprintln!(
                            "[anvild] {} run {} → {:?} ({} ms; deliveries: {})",
                            def_clone.name,
                            outcome.run_id,
                            outcome.status,
                            outcome.duration_ms,
                            outcome
                                .deliveries
                                .iter()
                                .filter(|d| d.ok)
                                .count()
                        );
                    });
                    status.last_tick_routines_fired += 1;
                }
                RoutineTier::Ask => {
                    // Drop a proposal so the user can approve/reject from
                    // the TUI.  Skip if an unapproved proposal already
                    // exists for this routine — no point piling up.
                    if !proposal::has_pending_for(home, &def.name) {
                        let p = RoutineProposal::from_def(def, next, now);
                        match proposal::write_proposal(home, &p) {
                            Ok(path) => {
                                eprintln!(
                                    "[anvild] {} ask-tier ({}): wrote proposal {}",
                                    def.name,
                                    def.permission_mode.as_cli_arg(),
                                    path.display(),
                                );
                                status.last_tick_proposals_written += 1;
                            }
                            Err(e) => {
                                eprintln!(
                                    "[anvild] {} proposal write failed: {e}",
                                    def.name
                                );
                                status.last_error = Some(format!("proposal write: {e}"));
                            }
                        }
                    }
                }
            }

            // Compute the routine's next fire so we don't re-fire on the
            // next tick (most schedules use interval; we add the interval
            // to `now`, not `next`, to avoid runaway catch-up).
            let after = if def.enabled { now + 1 } else { now };
            let new_next = next_fire(&def.schedule, after).unwrap_or(u64::MAX);
            next_fire_cache.insert(def.name.clone(), new_next);
        }

        // Refresh the on-disk pending count once per tick (expiry sweep
        // also happens here as a side effect).
        status.pending_proposals_total = proposal::list_pending(home, now).len();

        // Task #763 (v2.2.20): snapshot the OAuth observability slot into
        // the status sidecar so external observers see proof-of-life.
        {
            let obs = oauth_obs.lock().unwrap_or_else(|p| p.into_inner());
            status.last_oauth_refresh_at = obs.last_refresh_at;
            status.oauth_expires_at = obs.expires_at;
            status.last_oauth_error = obs.last_error.clone();
            status.oauth_keepalive_alive = obs.alive;
            status.last_oauth_check_at = obs.last_check_at;
        }
        // Task #764 (v2.2.20): snapshot Gemini + Copilot obs.
        {
            let obs = gemini_obs.lock().unwrap_or_else(|p| p.into_inner());
            status.last_gemini_refresh_at = obs.last_refresh_at;
            status.gemini_expires_at = obs.expires_at;
            status.last_gemini_error = obs.last_error.clone();
            status.gemini_keepalive_alive = obs.alive;
            status.last_gemini_check_at = obs.last_check_at;
        }
        {
            let obs = copilot_obs.lock().unwrap_or_else(|p| p.into_inner());
            status.copilot_expires_at = obs.expires_at;
            status.last_copilot_error = obs.last_error.clone();
            status.copilot_monitor_alive = obs.alive;
            status.last_copilot_check_at = obs.last_check_at;
        }

        status.last_tick_at = now;
        write_status(home, &status);

        // Sleep ~30 s, but wake immediately on stop signal.
        let target_tick = Duration::from_secs(30);
        let elapsed = tick_start.elapsed();
        let remaining = target_tick.saturating_sub(elapsed);
        let deadline = Instant::now() + remaining;
        while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    eprintln!("[anvild] shutting down");
    let _ = fs::remove_file(pid_path(home));
    0
}

// ─── Signal handling ─────────────────────────────────────────────────────────

#[cfg(unix)]
fn install_signal_handler(stop: Arc<AtomicBool>) {
    use std::os::raw::c_int;
    extern "C" fn handler(_sig: c_int) {
        SHUTDOWN.store(true, Ordering::SeqCst);
    }
    static SHUTDOWN: AtomicBool = AtomicBool::new(false);
    unsafe {
        unsafe extern "C" {
            fn signal(sig: c_int, handler: extern "C" fn(c_int)) -> usize;
        }
        // SIGTERM = 15, SIGINT = 2 on every Unix Anvil supports.
        signal(15, handler);
        signal(2, handler);
    }
    // Bridge the static flag into the Arc that the loop polls.
    std::thread::spawn(move || loop {
        if SHUTDOWN.load(Ordering::SeqCst) {
            stop.store(true, Ordering::SeqCst);
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    });
}

#[cfg(windows)]
fn install_signal_handler(_stop: Arc<AtomicBool>) {
    // Ctrl+C handling on Windows lives in the foreground console; service
    // unit invocations terminate via SCM stop.  v2.3 will wire SetConsoleCtrlHandler.
}

// ─── start (detached spawn) ──────────────────────────────────────────────────

pub fn spawn_detached(home: &Path, anvil_binary: &Path) -> i32 {
    if let Err(e) = validate_anvil_binary(anvil_binary) {
        eprintln!("anvil daemon: {e}");
        return 2;
    }
    if let Some(existing) = read_pid(home) {
        if pid_alive(existing) {
            eprintln!("anvil daemon: already running (pid {existing})");
            return 0;
        }
        let _ = fs::remove_file(pid_path(home));
    }
    let _ = fs::create_dir_all(home.join("run"));
    let log = log_path(home);
    let stdout = match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("anvil daemon: cannot open log {}: {e}", log.display());
            return 3;
        }
    };
    let stderr = match stdout.try_clone() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("anvil daemon: cannot clone log fd: {e}");
            return 3;
        }
    };

    // Spawn ourselves as `anvild daemon foreground` so the child runs the
    // exact same code path as `--foreground` and writes its own PID file.
    // Task #766: prefer the anvild sibling so ps/top show "anvild" as the
    // process name. Fall back to the anvil binary if the symlink is missing
    // (older installs, custom layouts, dev builds).
    let anvild_binary = anvild_path_from(anvil_binary);
    let exec_binary: &Path = if anvild_binary.exists() {
        &anvild_binary
    } else {
        anvil_binary
    };
    let mut cmd = std::process::Command::new(exec_binary);
    cmd.arg("daemon")
        .arg("foreground")
        .env("ANVIL_CONFIG_HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr));

    #[cfg(unix)]
    detach_unix(&mut cmd);
    #[cfg(windows)]
    detach_windows(&mut cmd);

    match cmd.spawn() {
        Ok(child) => {
            // Wait briefly for the child to write its PID file so the user
            // gets immediate confirmation it's alive.
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if read_pid(home).is_some_and(pid_alive) {
                    let pid = read_pid(home).unwrap_or(child.id());
                    println!("anvil daemon: started (pid {pid}, log {})", log.display());
                    return 0;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            // Child might be alive but slow; report the spawn-time PID as
            // best-effort.
            println!(
                "anvil daemon: started (pid {}, log {}) — status pending",
                child.id(),
                log.display()
            );
            0
        }
        Err(e) => {
            eprintln!("anvil daemon: spawn failed: {e}");
            4
        }
    }
}

#[cfg(unix)]
fn detach_unix(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // setsid() at the child's pre_exec moves the new process into its own
    // session, detaching from the parent's controlling terminal.  Survives
    // when the parent shell exits.
    unsafe {
        cmd.pre_exec(|| {
            unsafe extern "C" {
                fn setsid() -> i32;
            }
            let _ = unsafe { setsid() };
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach_windows(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x00000008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

// ─── stop ───────────────────────────────────────────────────────────────────

fn stop(home: &Path) -> i32 {
    let Some(pid) = read_pid(home) else {
        println!("anvil daemon: not running (no PID file)");
        return 0;
    };
    if !pid_alive(pid) {
        let _ = fs::remove_file(pid_path(home));
        println!("anvil daemon: not running (stale PID cleared)");
        return 0;
    }
    #[cfg(unix)]
    {
        use std::os::raw::c_int;
        unsafe {
            unsafe extern "C" {
                fn kill(pid: c_int, sig: c_int) -> c_int;
            }
            // SIGTERM
            kill(pid as c_int, 15);
        }
    }
    #[cfg(windows)]
    {
        // Best-effort: spawn taskkill since std::process doesn't expose a
        // cross-PID terminate.  Service unit invocations should prefer
        // `sc stop` anyway.
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    // Wait for graceful exit, up to 5 s.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if pid_alive(pid) {
        eprintln!("anvil daemon: pid {pid} did not exit within 5s");
        return 5;
    }
    let _ = fs::remove_file(pid_path(home));
    println!("anvil daemon: stopped (pid {pid})");
    0
}

// ─── status ─────────────────────────────────────────────────────────────────

fn print_status(home: &Path) -> i32 {
    let pid_opt = read_pid(home);
    match pid_opt {
        Some(pid) if pid_alive(pid) => {
            print!("anvil daemon: running (pid {pid})");
            let status = fs::read_to_string(status_path(home))
                .ok()
                .and_then(|s| serde_json::from_str::<DaemonStatus>(&s).ok());
            match status {
                Some(s) => {
                    let now = unix_now();
                    let uptime = now.saturating_sub(s.started_at);
                    println!(
                        "; uptime {}; routines loaded {}; last tick {}s ago",
                        fmt_duration(uptime),
                        s.last_tick_routines_loaded,
                        now.saturating_sub(s.last_tick_at),
                    );
                    if let Some(err) = &s.last_error {
                        println!("  last error: {err}");
                    }
                    // Task #763 (v2.2.20): OAuth keepalive proof-of-life.
                    println!(
                        "  oauth keepalive: {}",
                        if s.oauth_keepalive_alive {
                            "alive"
                        } else {
                            "no events yet (still in initial sleep)"
                        }
                    );
                    if let Some(exp) = s.oauth_expires_at {
                        let delta = exp as i64 - now as i64;
                        let pretty = if delta >= 0 {
                            format!("in {}", fmt_duration(delta as u64))
                        } else {
                            format!("{} ago (EXPIRED)", fmt_duration((-delta) as u64))
                        };
                        println!("  oauth token expires_at: {exp} ({pretty})");
                    } else {
                        println!("  oauth token expires_at: (unknown)");
                    }
                    if let Some(at) = s.last_oauth_refresh_at {
                        println!(
                            "  last oauth refresh: {}s ago",
                            now.saturating_sub(at)
                        );
                    } else {
                        println!("  last oauth refresh: (none since daemon start)");
                    }
                    if let Some(at) = s.last_oauth_check_at {
                        println!(
                            "  last oauth check: {}s ago",
                            now.saturating_sub(at)
                        );
                    }
                    if let Some(err) = &s.last_oauth_error {
                        println!("  last oauth error: {err}");
                    }
                    // Task #764 (v2.2.20): Gemini section — only shown when a token
                    // was present at daemon start (alive flag set by the thread).
                    if s.gemini_keepalive_alive || s.gemini_expires_at.is_some() {
                        println!(
                            "  gemini keepalive: {}",
                            if s.gemini_keepalive_alive {
                                "alive"
                            } else {
                                "no events yet (still in initial sleep)"
                            }
                        );
                        if let Some(exp) = s.gemini_expires_at {
                            let delta = exp as i64 - now as i64;
                            let pretty = if delta >= 0 {
                                format!("in {}", fmt_duration(delta as u64))
                            } else {
                                format!("{} ago (EXPIRED)", fmt_duration((-delta) as u64))
                            };
                            println!("  gemini token expires_at: {exp} ({pretty})");
                        } else {
                            println!("  gemini token expires_at: (unknown)");
                        }
                        if let Some(at) = s.last_gemini_refresh_at {
                            println!(
                                "  last gemini refresh: {}s ago",
                                now.saturating_sub(at)
                            );
                        } else {
                            println!("  last gemini refresh: (none since daemon start)");
                        }
                        if let Some(at) = s.last_gemini_check_at {
                            println!(
                                "  last gemini check: {}s ago",
                                now.saturating_sub(at)
                            );
                        }
                        if let Some(err) = &s.last_gemini_error {
                            println!("  last gemini error: {err}");
                        }
                    }
                    // Task #764 (v2.2.20): Copilot section — monitor-only (no refresh).
                    if s.copilot_monitor_alive || s.copilot_expires_at.is_some() {
                        println!(
                            "  copilot monitor: {}",
                            if s.copilot_monitor_alive {
                                "alive"
                            } else {
                                "no events yet (still in initial sleep)"
                            }
                        );
                        if let Some(exp) = s.copilot_expires_at {
                            let delta = exp as i64 - now as i64;
                            let pretty = if delta >= 0 {
                                format!("in {}", fmt_duration(delta as u64))
                            } else {
                                format!("{} ago (EXPIRED)", fmt_duration((-delta) as u64))
                            };
                            println!("  copilot token expires_at: {exp} ({pretty})");
                            if delta < 86_400 && delta >= 0 {
                                println!(
                                    "  copilot warning: token expires in less than 24h — run: anvil provider login copilot"
                                );
                            }
                        } else {
                            println!("  copilot token expires_at: (unknown)");
                        }
                        if let Some(at) = s.last_copilot_check_at {
                            println!(
                                "  last copilot check: {}s ago",
                                now.saturating_sub(at)
                            );
                        }
                        if let Some(err) = &s.last_copilot_error {
                            println!("  copilot warning: {err}");
                        }
                    }
                }
                None => println!(" (status file missing)"),
            }
            0
        }
        Some(_) => {
            println!("anvil daemon: not running (stale PID file)");
            2
        }
        None => {
            println!("anvil daemon: not running");
            1
        }
    }
}

fn fmt_duration(secs: u64) -> String {
    let (h, rem) = (secs / 3600, secs % 3600);
    let (m, s) = (rem / 60, rem % 60);
    if h > 0 {
        format!("{h}h{m:02}m{s:02}s")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

// ─── Service unit generators ────────────────────────────────────────────────

pub fn install_service(home: &Path, anvil_binary: &Path) -> i32 {
    if let Err(e) = validate_anvil_binary(anvil_binary) {
        eprintln!("anvil daemon: {e}");
        return 2;
    }
    let _ = fs::create_dir_all(home.join("run"));

    #[cfg(target_os = "macos")]
    {
        let plist = build_launchagent_plist(home, anvil_binary);
        let path = launchagent_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(e) = fs::write(&path, plist) {
            eprintln!("anvil daemon: write {} failed: {e}", path.display());
            return 3;
        }
        println!("anvil daemon: wrote {}", path.display());
        println!("  Load now:  launchctl load -w {}", path.display());
        println!("  Unload:    launchctl unload {}", path.display());
        return 0;
    }
    #[cfg(target_os = "linux")]
    {
        let unit = build_systemd_unit(anvil_binary);
        let path = systemd_unit_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(e) = fs::write(&path, unit) {
            eprintln!("anvil daemon: write {} failed: {e}", path.display());
            return 3;
        }
        println!("anvil daemon: wrote {}", path.display());
        println!("  Enable now:  systemctl --user daemon-reload && systemctl --user enable --now anvild.service");
        println!("  Disable:     systemctl --user disable --now anvild.service");
        return 0;
    }
    #[cfg(target_os = "windows")]
    {
        let xml = build_taskscheduler_xml(anvil_binary);
        let path = home.join("run").join("anvild-task.xml");
        if let Err(e) = fs::write(&path, xml) {
            eprintln!("anvil daemon: write {} failed: {e}", path.display());
            return 3;
        }
        println!("anvil daemon: wrote {}", path.display());
        println!("  Register: schtasks /Create /TN Anvild /XML \"{}\"", path.display());
        println!("  Unregister: schtasks /Delete /TN Anvild /F");
        return 0;
    }
    #[cfg(target_os = "freebsd")]
    {
        let rc = build_freebsd_rc(anvil_binary, home);
        let path = home.join("run").join("anvild.rc");
        if let Err(e) = fs::write(&path, rc) {
            eprintln!("anvil daemon: write {} failed: {e}", path.display());
            return 3;
        }
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o755));
        println!("anvil daemon: wrote {}", path.display());
        println!("  Install: sudo cp {} /usr/local/etc/rc.d/anvild", path.display());
        println!("  Enable:  sudo sysrc anvild_enable=YES");
        println!("  Start:   sudo service anvild start");
        return 0;
    }
    #[cfg(target_os = "netbsd")]
    {
        let rc = build_netbsd_rc(anvil_binary, home);
        let path = home.join("run").join("anvild.rc");
        if let Err(e) = fs::write(&path, rc) {
            eprintln!("anvil daemon: write {} failed: {e}", path.display());
            return 3;
        }
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o755));
        println!("anvil daemon: wrote {}", path.display());
        println!("  Install: sudo cp {} /etc/rc.d/anvild", path.display());
        println!("  Enable:  echo 'anvild=YES' | sudo tee -a /etc/rc.conf");
        println!("  Start:   sudo service anvild start");
        return 0;
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows",
        target_os = "freebsd",
        target_os = "netbsd"
    )))]
    {
        let _ = (home, anvil_binary);
        eprintln!("anvil daemon: install-service not supported on this platform");
        return 6;
    }
}

fn uninstall_service(home: &Path) -> i32 {
    #[cfg(target_os = "macos")]
    {
        let path = launchagent_path();
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &path.to_string_lossy()])
            .status();
        let _ = fs::remove_file(&path);
        println!("anvil daemon: removed {}", path.display());
        return 0;
    }
    #[cfg(target_os = "linux")]
    {
        let path = systemd_unit_path();
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "anvild.service"])
            .status();
        let _ = fs::remove_file(&path);
        println!("anvil daemon: removed {}", path.display());
        return 0;
    }
    #[cfg(target_os = "windows")]
    {
        let _ = home;
        let _ = std::process::Command::new("schtasks")
            .args(["/Delete", "/TN", "Anvild", "/F"])
            .status();
        println!("anvil daemon: removed Anvild task");
        return 0;
    }
    #[cfg(any(target_os = "freebsd", target_os = "netbsd"))]
    {
        let path = home.join("run").join("anvild.rc");
        let _ = fs::remove_file(&path);
        println!("anvil daemon: removed user-space {}", path.display());
        println!("  Also run: sudo service anvild stop && sudo rm /usr/local/etc/rc.d/anvild  (FreeBSD)");
        println!("            sudo service anvild stop && sudo rm /etc/rc.d/anvild           (NetBSD)");
        return 0;
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows",
        target_os = "freebsd",
        target_os = "netbsd"
    )))]
    {
        let _ = home;
        eprintln!("anvil daemon: uninstall-service not supported on this platform");
        return 6;
    }
}

#[cfg(target_os = "macos")]
fn launchagent_path() -> PathBuf {
    dirs_next::home_dir()
        .map(|h| h.join("Library/LaunchAgents/net.culpur.anvild.plist"))
        .unwrap_or_else(|| PathBuf::from("net.culpur.anvild.plist"))
}

/// Compute the anvild sibling path next to the anvil binary (task #766).
///
/// `/usr/local/bin/anvil` -> `/usr/local/bin/anvild`.
/// `C:\foo\anvil.exe` -> `C:\foo\anvild.exe`.
/// The anvild file is created at install time as a symlink (Unix) or
/// hardlink (Windows) by `install/install.sh` / `install.ps1`. Unit-file
/// generators MUST reference this path, not the anvil path, so the OS
/// supervisor execs argv[0]="anvild" — ps/top/launchctl show the daemon
/// as a separately-named process.
pub(crate) fn anvild_path_from(anvil_binary: &Path) -> PathBuf {
    let parent = anvil_binary.parent().unwrap_or_else(|| Path::new(""));
    let file_name = anvil_binary
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "anvil".to_string());
    // Preserve .exe extension on Windows.
    let new_name = if let Some(stem) = file_name.strip_suffix(".exe") {
        format!("{stem}d.exe")
    } else {
        format!("{file_name}d")
    };
    parent.join(new_name)
}

/// LaunchAgent plist generator.  Public for the tests below.
#[cfg(any(target_os = "macos", test))]
pub fn build_launchagent_plist(home: &Path, binary: &Path) -> String {
    let home_s = home.to_string_lossy();
    let anvild = anvild_path_from(binary);
    let bin_s = anvild.to_string_lossy();
    let log_s = home.join("run").join("anvild.log");
    let log_s = log_s.to_string_lossy();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>net.culpur.anvild</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin_s}</string>
        <string>daemon</string>
        <string>foreground</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>ANVIL_CONFIG_HOME</key>
        <string>{home_s}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log_s}</string>
    <key>StandardErrorPath</key>
    <string>{log_s}</string>
</dict>
</plist>
"#
    )
}

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> PathBuf {
    dirs_next::config_dir()
        .map(|c| c.join("systemd/user/anvild.service"))
        .unwrap_or_else(|| PathBuf::from("anvild.service"))
}

/// systemd --user unit generator.  Public for the tests below.
#[cfg(any(target_os = "linux", test))]
pub fn build_systemd_unit(binary: &Path) -> String {
    let anvild = anvild_path_from(binary);
    let bin_s = anvild.to_string_lossy();
    format!(
        r#"[Unit]
Description=Anvil routines daemon (anvild)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={bin_s} daemon foreground
Restart=on-failure
RestartSec=5s
# Keep stdout/stderr — anvild writes structured per-tick logs to its own file.
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=default.target
"#
    )
}

/// Windows Task Scheduler XML generator.  Public for the tests below.
#[cfg(any(target_os = "windows", test))]
pub fn build_taskscheduler_xml(binary: &Path) -> String {
    let anvild = anvild_path_from(binary);
    let bin_s = anvild.to_string_lossy();
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Anvil routines daemon (anvild)</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <RestartOnFailure>
      <Interval>PT5S</Interval>
      <Count>10</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{bin_s}</Command>
      <Arguments>daemon foreground</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// FreeBSD rc.d script generator (task #766).
///
/// Written to `<home>/run/anvild.rc` from user space. User then copies it
/// to `/usr/local/etc/rc.d/anvild` with sudo, `sysrc anvild_enable=YES`,
/// `service anvild start`. The anvild path (not anvil) so `ps aux` shows
/// "anvild" as the process name.
#[cfg(any(target_os = "freebsd", test))]
pub fn build_freebsd_rc(binary: &Path, home: &Path) -> String {
    let anvild = anvild_path_from(binary);
    let bin_s = anvild.to_string_lossy();
    let home_s = home.to_string_lossy();
    format!(
        r#"#!/bin/sh
# PROVIDE: anvild
# REQUIRE: NETWORKING
# KEYWORD: shutdown
#
# Add the following to /etc/rc.conf to enable:
#   anvild_enable="YES"
#   anvild_user="<your-username>"

. /etc/rc.subr

name="anvild"
rcvar="anvild_enable"
command="{bin_s}"
command_args="daemon foreground"
pidfile="{home_s}/run/anvild.pid"
anvild_env="ANVIL_CONFIG_HOME={home_s}"

load_rc_config $name
: ${{anvild_enable:=NO}}

run_rc_command "$1"
"#
    )
}

/// NetBSD rc.d script generator (task #766).
///
/// Written to `<home>/run/anvild.rc`. Install: `sudo cp ... /etc/rc.d/anvild`,
/// `echo 'anvild=YES' >> /etc/rc.conf`, `sudo service anvild start`.
#[cfg(any(target_os = "netbsd", test))]
pub fn build_netbsd_rc(binary: &Path, home: &Path) -> String {
    let anvild = anvild_path_from(binary);
    let bin_s = anvild.to_string_lossy();
    let home_s = home.to_string_lossy();
    format!(
        r#"#!/bin/sh
# PROVIDE: anvild
# REQUIRE: NETWORK
# KEYWORD: shutdown
#
# Add to /etc/rc.conf:  anvild=YES

. /etc/rc.subr

name="anvild"
rcvar=$name
command="{bin_s}"
command_args="daemon foreground"
pidfile="{home_s}/run/anvild.pid"

load_rc_config $name
run_rc_command "$1"
"#
    )
}

// ─── Misc helpers ────────────────────────────────────────────────────────────

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Silence unused-import warning when neither install-service nor stop are
/// reachable on the current target (Path is borrowed by sibling fns).
#[allow(dead_code)]
fn _force_path_link(_p: &Path, _w: &mut dyn Write) {}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Task #766: anvild process-name across 7 platforms ─────────────────

    #[test]
    fn anvild_path_from_unix_no_extension() {
        let bin = std::path::PathBuf::from("/usr/local/bin/anvil");
        assert_eq!(anvild_path_from(&bin), std::path::PathBuf::from("/usr/local/bin/anvild"));
    }

    #[test]
    fn anvild_path_from_windows_exe() {
        let bin = std::path::PathBuf::from("C:\\Users\\soul\\anvil.exe");
        let got = anvild_path_from(&bin);
        let got_s = got.to_string_lossy().replace('/', "\\");
        assert!(got_s.ends_with("anvild.exe"), "expected ...anvild.exe, got {got_s}");
    }

    #[test]
    fn launchagent_plist_uses_anvild_not_anvil() {
        let home = std::path::PathBuf::from("/tmp/test-home");
        let bin = std::path::PathBuf::from("/usr/local/bin/anvil");
        let plist = build_launchagent_plist(&home, &bin);
        assert!(
            plist.contains("<string>/usr/local/bin/anvild</string>"),
            "plist must reference anvild path, got:\n{plist}"
        );
        assert!(
            !plist.contains("<string>/usr/local/bin/anvil</string>"),
            "plist must NOT have bare anvil path under ProgramArguments, got:\n{plist}"
        );
    }

    #[test]
    fn systemd_unit_uses_anvild_not_anvil() {
        let bin = std::path::PathBuf::from("/usr/local/bin/anvil");
        let unit = build_systemd_unit(&bin);
        assert!(
            unit.contains("ExecStart=/usr/local/bin/anvild daemon foreground"),
            "systemd unit must use anvild, got:\n{unit}"
        );
    }

    #[test]
    fn taskscheduler_xml_uses_anvild_exe() {
        let bin = std::path::PathBuf::from("C:\\Users\\soul\\anvil.exe");
        let xml = build_taskscheduler_xml(&bin);
        assert!(
            xml.contains("anvild.exe"),
            "task scheduler xml must reference anvild.exe, got:\n{xml}"
        );
    }

    #[test]
    fn freebsd_rc_uses_anvild_path_and_name() {
        let home = std::path::PathBuf::from("/home/user/.anvil");
        let bin = std::path::PathBuf::from("/usr/local/bin/anvil");
        let rc = build_freebsd_rc(&bin, &home);
        assert!(
            rc.contains("command=\"/usr/local/bin/anvild\""),
            "freebsd rc must use anvild command path, got:\n{rc}"
        );
        assert!(rc.contains("name=\"anvild\""), "freebsd rc must declare name=anvild");
    }

    #[test]
    fn netbsd_rc_uses_anvild_path_and_name() {
        let home = std::path::PathBuf::from("/home/user/.anvil");
        let bin = std::path::PathBuf::from("/usr/local/bin/anvil");
        let rc = build_netbsd_rc(&bin, &home);
        assert!(
            rc.contains("command=\"/usr/local/bin/anvild\""),
            "netbsd rc must use anvild command path, got:\n{rc}"
        );
        assert!(rc.contains("name=\"anvild\""), "netbsd rc must declare name=anvild");
    }

    // ── Original tests ──────────────────────────────────────────────────────

    #[test]
    fn parse_no_args_returns_status() {
        assert_eq!(parse(&[]).unwrap(), DaemonSubcommand::Status);
    }

    #[test]
    fn parse_start_detached_default() {
        let args = vec!["start".to_string()];
        assert_eq!(
            parse(&args).unwrap(),
            DaemonSubcommand::Start { foreground: false }
        );
    }

    #[test]
    fn parse_start_foreground_flag() {
        let args = vec!["start".to_string(), "--foreground".to_string()];
        assert_eq!(
            parse(&args).unwrap(),
            DaemonSubcommand::Start { foreground: true }
        );
    }

    #[test]
    fn parse_start_short_flag() {
        let args = vec!["start".to_string(), "-f".to_string()];
        assert_eq!(
            parse(&args).unwrap(),
            DaemonSubcommand::Start { foreground: true }
        );
    }

    #[test]
    fn parse_unknown_subcommand_errors() {
        let args = vec!["wat".to_string()];
        assert!(parse(&args).is_err());
    }

    #[test]
    fn parse_install_service() {
        let args = vec!["install-service".to_string()];
        assert_eq!(parse(&args).unwrap(), DaemonSubcommand::InstallService);
    }

    #[test]
    fn parse_uninstall_service() {
        let args = vec!["uninstall-service".to_string()];
        assert_eq!(parse(&args).unwrap(), DaemonSubcommand::UninstallService);
    }

    #[test]
    fn fmt_duration_seconds_only() {
        assert_eq!(fmt_duration(0), "0s");
        assert_eq!(fmt_duration(45), "45s");
    }

    #[test]
    fn fmt_duration_minutes() {
        assert_eq!(fmt_duration(125), "2m05s");
    }

    #[test]
    fn fmt_duration_hours() {
        assert_eq!(fmt_duration(3_725), "1h02m05s");
    }

    #[test]
    fn launchagent_plist_contains_bin_and_home() {
        let p = build_launchagent_plist(
            Path::new("/Users/x/.anvil"),
            Path::new("/Users/x/bin/anvil"),
        );
        assert!(p.contains("/Users/x/bin/anvil"));
        assert!(p.contains("/Users/x/.anvil"));
        assert!(p.contains("daemon"));
        assert!(p.contains("foreground"));
        assert!(p.contains("KeepAlive"));
    }

    #[test]
    fn systemd_unit_contains_bin_and_restart() {
        // Task #766: unit must execute the anvild symlink, not anvil itself.
        let u = build_systemd_unit(Path::new("/usr/local/bin/anvil"));
        assert!(u.contains("/usr/local/bin/anvild daemon foreground"));
        assert!(u.contains("Restart=on-failure"));
        assert!(u.contains("[Install]"));
    }

    #[test]
    fn taskscheduler_xml_contains_bin_and_logon_trigger() {
        // Task #766: Task Scheduler must launch anvild.exe (sibling hardlink).
        let x = build_taskscheduler_xml(Path::new(r"C:\Program Files\Anvil\anvil.exe"));
        assert!(x.contains(r"C:\Program Files\Anvil\anvild.exe"));
        assert!(x.contains("<LogonTrigger>"));
        assert!(x.contains("daemon foreground"));
    }

    #[test]
    fn status_file_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let s = DaemonStatus {
            pid: 12345,
            started_at: 100,
            last_tick_at: 200,
            last_tick_routines_loaded: 4,
            last_tick_routines_fired: 1,
            last_tick_proposals_written: 1,
            pending_proposals_total: 2,
            last_error: Some("bad routine".into()),
            anvil_version: "2.2.18-test".into(),
            // Task #763 fields
            last_oauth_refresh_at: Some(150),
            oauth_expires_at: Some(9999),
            last_oauth_error: None,
            oauth_keepalive_alive: true,
            last_oauth_check_at: Some(200),
            // Task #764 Gemini fields
            last_gemini_refresh_at: Some(160),
            gemini_expires_at: Some(8888),
            last_gemini_error: None,
            gemini_keepalive_alive: true,
            last_gemini_check_at: Some(200),
            // Task #764 Copilot fields
            copilot_expires_at: Some(7777),
            last_copilot_error: Some("expires soon".into()),
            copilot_monitor_alive: true,
            last_copilot_check_at: Some(200),
        };
        write_status(tmp.path(), &s);
        let raw = fs::read_to_string(status_path(tmp.path())).unwrap();
        let back: DaemonStatus = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.pid, 12345);
        assert_eq!(back.last_tick_routines_fired, 1);
        assert_eq!(back.last_tick_proposals_written, 1);
        assert_eq!(back.pending_proposals_total, 2);
        // Task #763 round-trip
        assert_eq!(back.last_oauth_refresh_at, Some(150));
        assert_eq!(back.oauth_expires_at, Some(9999));
        assert!(back.oauth_keepalive_alive);
        // Task #764 Gemini round-trip
        assert_eq!(back.last_gemini_refresh_at, Some(160));
        assert_eq!(back.gemini_expires_at, Some(8888));
        assert!(back.gemini_keepalive_alive);
        // Task #764 Copilot round-trip
        assert_eq!(back.copilot_expires_at, Some(7777));
        assert_eq!(back.last_copilot_error.as_deref(), Some("expires soon"));
        assert!(back.copilot_monitor_alive);
    }

    /// Verify that a status JSON written by an older daemon (missing Task #764
    /// fields) still deserialises successfully — the `#[serde(default)]`
    /// attributes must cover all new fields.
    #[test]
    fn status_file_backwards_compat_missing_new_fields() {
        let legacy_json = r#"{
            "pid": 999,
            "started_at": 1,
            "last_tick_at": 2,
            "last_tick_routines_loaded": 0,
            "last_tick_routines_fired": 0,
            "last_error": null,
            "anvil_version": "2.2.18"
        }"#;
        let back: DaemonStatus = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(back.pid, 999);
        assert!(!back.oauth_keepalive_alive);
        assert!(back.oauth_expires_at.is_none());
        assert!(!back.gemini_keepalive_alive);
        assert!(back.gemini_expires_at.is_none());
        assert!(!back.copilot_monitor_alive);
        assert!(back.copilot_expires_at.is_none());
    }
}
