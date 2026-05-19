// Edition 2024: env::set_var/remove_var require unsafe
#![allow(unsafe_code)]

// Task #626 — `/remote-control` / `/share` is dispatched from inside the
// TUI; the spawned relay thread previously `eprintln!`-ed errors into
// ratatui's back-buffer.  Warnings now route through `tui::log_warning`.
#![deny(clippy::print_stdout, clippy::print_stderr)]

//! Remote control command handler for `impl LiveCli`.
//!
//! Extracted from `main.rs` to reduce file size.  The `LiveCli` struct and its
//! relay-related fields remain in `main.rs`; only the command implementation
//! lives here.

use crate::LiveCli;

impl LiveCli {
    /// `/remote-control [stop|status]` — manage the web viewer relay session.
    pub(crate) fn run_remote_control_command(&mut self, action: Option<&str>) -> String {
        const HUB_BASE_URL: &str = "https://passage.culpur.net/viewer";

        match action.map_or("", str::trim) {
            "stop" => {
                if self.relay_session.is_none() {
                    return "Remote control: no active session.".to_string();
                }
                self.relay_session = None;
                self.relay_event_tx = None;
                self.relay_input_rx = None;
                "Remote control: session stopped.".to_string()
            }
            "status" => {
                match &self.relay_session {
                    None => "Remote control: no active session.".to_string(),
                    Some(session) => {
                        let client_count = self
                            .relay_event_tx
                            .as_ref()
                            .map_or(0, tokio::sync::broadcast::Sender::receiver_count);
                        format!(
                            "Remote control\n  URL              {}\n  Hash             {}\n  Clients          {}\n  Status           {:?}\n\nNext\n  /remote-control stop   Stop the relay session",
                            session.url,
                            session.hash,
                            client_count,
                            session.status,
                        )
                    }
                }
            }
            // default: start (or report existing session)
            _ => {
                if let Some(session) = &self.relay_session {
                    return format!(
                        "Remote control is already active.\n  URL    {}\n  Hash   {}\n\nUse /remote-control status  to see details.\nUse /remote-control stop    to end the session.",
                        session.url, session.hash
                    );
                }

                let hash = runtime::relay::generate_session_hash();
                let pairing_code = runtime::relay::generate_pairing_code();
                let mut session = runtime::relay::RelaySession::new(hash.clone(), HUB_BASE_URL);
                let url = session.url.clone();
                let (event_tx, _) = tokio::sync::broadcast::channel::<runtime::relay::RelayMessage>(256);

                // Create the relay host with pairing code display channel
                let (code_display_tx, _code_display_rx) = tokio::sync::mpsc::unbounded_channel();
                let relay_host = runtime::relay::RelayHost::new(
                    hash.clone(),
                    HUB_BASE_URL,
                    code_display_tx,
                );

                // Subscribe to the event broadcast for the relay WS loop
                let event_rx = event_tx.subscribe();

                // Spawn the relay WebSocket connection on a background thread
                // using a dedicated tokio runtime (the provider's runtime may not be available)
                let passage_ws_url = "wss://api.culpur.net/v1/relay/sessions".to_string();
                let pairing_code_for_relay = pairing_code.clone();
                // Create a sync channel for receiving user messages from web clients
                let (relay_input_tx, relay_input_rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    else {
                        // Task #626: relay thread fires while TUI alt-screen
                        // is up — route through the TUI-aware warning sink.
                        crate::tui::log_warning("Failed to create relay tokio runtime");
                        return;
                    };
                    // Set the fixed pairing code BEFORE running the relay
                    rt.block_on(relay_host.set_pairing_code(pairing_code_for_relay));
                    let snapshot_fn = std::sync::Arc::new(tokio::sync::Mutex::new(
                        None::<Box<dyn Fn() -> Vec<runtime::relay::TabSnapshot> + Send>>,
                    ));
                    if let Err(e) = rt.block_on(relay_host.run(&passage_ws_url, event_rx, snapshot_fn, Some(relay_input_tx))) {
                        // Task #626: same TUI-aware sink for the disconnect
                        // path — without this the back-buffer corrupts.
                        crate::tui::log_warning(&format!("Relay disconnected: {e}"));
                    }
                });

                // Task #647 (G6 + G7): spawn the daemon-status + proposal
                // poller.  Reads ~/.anvil/run/anvild.status.json and
                // runtime::routines::proposal::list_pending every
                // POLL_INTERVAL_SECS and emits over the same event_tx
                // broadcast.  Quits when the broadcast has zero receivers
                // (relay session was dropped).
                let poller_event_tx = event_tx.clone();
                std::thread::spawn(move || {
                    spawn_daemon_status_poller(poller_event_tx);
                });

                session.status = runtime::relay::RelayStatus::WaitingForClient;
                session.pairing_code = pairing_code.clone();
                self.relay_session = Some(session);
                self.relay_event_tx = Some(event_tx);
                self.relay_input_rx = Some(relay_input_rx);

                // Auto-open the viewer URL in the default browser
                let _ = open::that(&url);

                format!(
                    "Remote control started\n  URL              {url}\n  Pairing code     {pairing_code}\n  Hash             {hash}\n\nThe URL has been opened in your default browser.\nEnter the pairing code when prompted.\n\nNext\n  /remote-control status   Check connection status\n  /remote-control stop     Stop the relay session"
                )
            }
        }
    }
}

// ─── Daemon-status + proposal poller (task #647 G6 + G7) ───────────────────

use runtime::relay::{ProposalSummary, RelayMessage};
use runtime::relay_daemon::{DaemonFeedState, ProposalFeedState, POLL_INTERVAL_SECS};
use runtime::routines::proposal;
use std::time::Duration;

/// Run the daemon-status + proposal poller on the current thread.
///
/// Reads `~/.anvil/run/anvild.status.json` and the pending-proposals
/// directory every [`POLL_INTERVAL_SECS`] seconds; pushes a
/// `DaemonStatus` frame only when the body changes (or the idle
/// heartbeat fires) and a `ProposalSnapshot` / `ProposalAdded` /
/// `ProposalDropped` sequence keyed off the previous observation.
///
/// Quits when the broadcast channel reports zero receivers (the relay
/// session was dropped).
fn spawn_daemon_status_poller(event_tx: tokio::sync::broadcast::Sender<RelayMessage>) {
    let mut daemon_feed = DaemonFeedState::new();
    let mut proposal_feed = ProposalFeedState::new();
    // First poll must emit a snapshot regardless of dedupe.
    daemon_feed.force_next();
    proposal_feed.reset_for_new_pair();

    loop {
        if event_tx.receiver_count() == 0 {
            return;
        }

        let now = unix_now();
        let home = anvil_home();
        let status_msg = read_daemon_status(&home);
        if let Some(msg) = daemon_feed.observe(status_msg, now) {
            let _ = event_tx.send(msg);
        }

        let pending = proposal::list_pending(&home, now);
        let summaries: Vec<ProposalSummary> = pending
            .iter()
            .map(|p| ProposalSummary {
                routine: p.routine.clone(),
                schedule_raw: p.schedule_raw.clone(),
                permission_mode: p.permission_mode.as_cli_arg().to_string(),
                prompt_preview: p.prompt_preview.clone(),
                scheduled_at: p.scheduled_at,
                proposed_at: p.proposed_at,
            })
            .collect();
        for msg in proposal_feed.observe(summaries.iter()) {
            let _ = event_tx.send(msg);
        }

        std::thread::sleep(Duration::from_secs(POLL_INTERVAL_SECS));
    }
}

/// Read `~/.anvil/run/anvild.status.json` and translate it into a
/// `RelayMessage::DaemonStatus`.  Missing file or unreadable JSON →
/// `running = false`.
fn read_daemon_status(home: &std::path::Path) -> RelayMessage {
    let path = home.join("run").join("anvild.status.json");
    let raw = std::fs::read_to_string(&path).ok();
    let parsed: Option<serde_json::Value> =
        raw.as_deref().and_then(|s| serde_json::from_str(s).ok());

    let Some(json) = parsed else {
        return RelayMessage::DaemonStatus {
            running: false,
            pid: None,
            last_tick_at: None,
            routines_loaded: 0,
            routines_fired_last_tick: 0,
            pending_proposals_total: 0,
            last_error: None,
            anvil_version: None,
        };
    };

    let pid = json
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as u32);
    // A stale status file with a dead PID counts as not-running.
    let running = pid.is_some_and(pid_alive);

    RelayMessage::DaemonStatus {
        running,
        pid,
        last_tick_at: json.get("last_tick_at").and_then(serde_json::Value::as_u64),
        routines_loaded: json
            .get("last_tick_routines_loaded")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize,
        routines_fired_last_tick: json
            .get("last_tick_routines_fired")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize,
        pending_proposals_total: json
            .get("pending_proposals_total")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize,
        last_error: json
            .get("last_error")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        anvil_version: json
            .get("anvil_version")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    }
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    use std::os::raw::c_int;
    // SAFETY: signal 0 only probes process existence; no memory access.
    unsafe {
        unsafe extern "C" {
            fn kill(pid: c_int, sig: c_int) -> c_int;
        }
        kill(pid as c_int, 0) == 0
    }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    // Windows handling lives in daemon.rs; for the poller a stale PID
    // shows running=false until the user re-runs `anvil daemon start`.
    false
}

fn anvil_home() -> std::path::PathBuf {
    if let Ok(v) = std::env::var("ANVIL_CONFIG_HOME") {
        if !v.is_empty() {
            return std::path::PathBuf::from(v);
        }
    }
    if let Ok(v) = std::env::var("ANVIL_HOME") {
        if !v.is_empty() {
            return std::path::PathBuf::from(v);
        }
    }
    dirs_next::home_dir()
        .map(|h| h.join(".anvil"))
        .unwrap_or_else(|| std::path::PathBuf::from(".anvil"))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_daemon_status_missing_file_says_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        match read_daemon_status(tmp.path()) {
            RelayMessage::DaemonStatus { running, pid, .. } => {
                assert!(!running);
                assert_eq!(pid, None);
            }
            other => panic!("expected DaemonStatus, got {other:?}"),
        }
    }

    #[test]
    fn read_daemon_status_parses_status_file_dead_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let status_json = serde_json::json!({
            "pid": 999_999_999u64,           // very unlikely to be alive
            "started_at": 0,
            "last_tick_at": 12_345,
            "last_tick_routines_loaded": 3,
            "last_tick_routines_fired": 0,
            "pending_proposals_total": 2,
            "last_error": null,
            "anvil_version": "2.2.18-test",
        });
        std::fs::write(
            run_dir.join("anvild.status.json"),
            serde_json::to_string(&status_json).unwrap(),
        )
        .unwrap();

        match read_daemon_status(tmp.path()) {
            RelayMessage::DaemonStatus {
                running,
                routines_loaded,
                pending_proposals_total,
                last_tick_at,
                anvil_version,
                ..
            } => {
                assert!(!running, "dead PID should report not running");
                assert_eq!(routines_loaded, 3);
                assert_eq!(pending_proposals_total, 2);
                assert_eq!(last_tick_at, Some(12_345));
                assert_eq!(anvil_version.as_deref(), Some("2.2.18-test"));
            }
            other => panic!("expected DaemonStatus, got {other:?}"),
        }
    }
}
