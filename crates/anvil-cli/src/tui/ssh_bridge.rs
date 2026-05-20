//! Async-to-sync bridge for the embedded SSH client (T5-Ssh-D).
//!
//! `runtime::ssh::connect` is fully async (russh requires tokio). The TUI
//! main loop is sync. This module spawns a tokio runtime in a dedicated
//! thread and bridges the four `tokio::sync::mpsc` channels of an
//! `SshSession` onto plain `std::sync::mpsc` channels the TUI can drive
//! with `try_recv()`.
//!
//! It also flattens the `runtime::ssh::SshEvent` enum into a smaller
//! `UiSshEvent` that drops the keyboard-interactive variant (we don't
//! support that prompt UI in v2.2.12 — Phase F or v2.3 will).

use std::sync::mpsc as smpsc;

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use runtime::relay::RelayMessage;
use runtime::ssh::{connect, SshConfig, SshEvent};
use tokio::sync::broadcast;

/// Trimmed event stream the SSH tab consumes. We don't surface
/// `Connecting` (the tab opens in that state already), `AuthSuccess`
/// (immediately followed by `Connected`), or `InteractivePrompt` (no UI
/// for it yet).
#[derive(Debug, Clone)]
pub enum UiSshEvent {
    AuthAttempt(String),
    AuthFailed(String),
    Connected,
    Disconnected(Option<String>),
    Error(String),
}

/// Channels the spawned bridge hands back to the caller. The TUI puts
/// the receivers into [`crate::tui::ssh_tab::SshTabState`] and uses the
/// senders for stdin / resize.
pub struct SshChannels {
    pub stdin_tx: smpsc::Sender<Vec<u8>>,
    pub stdout_rx: smpsc::Receiver<Vec<u8>>,
    pub resize_tx: smpsc::Sender<(u32, u32)>,
    pub events_rx: smpsc::Receiver<UiSshEvent>,
}

/// Spawn a dedicated tokio runtime + bridge tasks for one SSH connection.
/// Returns sync channels immediately; the connection itself runs in the
/// background. Auth failures and connect errors flow through `events_rx`,
/// not the return value.
///
/// The thread terminates when the runtime SshSession's stdout channel
/// closes (server EOF or local error).
pub fn spawn_session(config: SshConfig, initial_size: (u32, u32)) -> SshChannels {
    let (stdin_tx, stdin_rx) = smpsc::channel::<Vec<u8>>();
    let (stdout_tx, stdout_rx) = smpsc::channel::<Vec<u8>>();
    let (resize_tx, resize_rx) = smpsc::channel::<(u32, u32)>();
    let (events_tx, events_rx) = smpsc::channel::<UiSshEvent>();

    std::thread::spawn(move || {
        // Build a single-thread tokio runtime — light, isolated, and we
        // don't need parallelism within one SSH session.
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = events_tx.send(UiSshEvent::Error(format!(
                    "tokio runtime build failed: {e}"
                )));
                return;
            }
        };

        rt.block_on(async move {
            let mut session = match connect(config, initial_size).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = events_tx.send(UiSshEvent::Error(e.to_string()));
                    return;
                }
            };

            // Spin three concurrent pumps:
            //   1. sync stdin_rx → async session.stdin
            //   2. async session.stdout → sync stdout_tx
            //   3. sync resize_rx → async session.resize
            // Plus the events relay (async session.events → sync events_tx).
            let stdin_to_remote = {
                let session_stdin = session.stdin.clone();
                tokio::task::spawn_blocking(move || {
                    while let Ok(bytes) = stdin_rx.recv() {
                        if session_stdin.blocking_send(bytes).is_err() {
                            break;
                        }
                    }
                })
            };

            let resize_to_remote = {
                let session_resize = session.resize.clone();
                tokio::task::spawn_blocking(move || {
                    while let Ok(size) = resize_rx.recv() {
                        if session_resize.blocking_send(size).is_err() {
                            break;
                        }
                    }
                })
            };

            // Events relay
            let events_relay_tx = events_tx.clone();
            let events_relay = tokio::spawn(async move {
                while let Some(ev) = session.events.recv().await {
                    let ui = match ev {
                        SshEvent::AuthAttempt { method } => {
                            UiSshEvent::AuthAttempt(method.to_string())
                        }
                        SshEvent::AuthFailure(reason) => UiSshEvent::AuthFailed(reason),
                        SshEvent::Connected => UiSshEvent::Connected,
                        SshEvent::Disconnected(reason) => UiSshEvent::Disconnected(reason),
                        SshEvent::Error(msg) => UiSshEvent::Error(msg),
                        // Skipped: Connecting, AuthSuccess, InteractivePrompt
                        _ => continue,
                    };
                    if events_relay_tx.send(ui).is_err() {
                        break;
                    }
                }
            });

            // Stdout pump (drives this task to completion).
            while let Some(chunk) = session.stdout.recv().await {
                if stdout_tx.send(chunk).is_err() {
                    break;
                }
            }

            // stdout closed → channel done. Best-effort tell UI; ignore
            // if events_relay already sent Disconnected.
            let _ = events_tx.send(UiSshEvent::Disconnected(None));

            // Pumps will exit naturally as their channels drop.
            let _ = stdin_to_remote.await;
            let _ = resize_to_remote.await;
            let _ = events_relay.await;
        });
    });

    SshChannels {
        stdin_tx,
        stdout_rx,
        resize_tx,
        events_rx,
    }
}

// ─── Phase 1 (#706): remote SSH session bridge ──────────────────────────────
//
// The TUI bridge above moves bytes between russh and a sync `mpsc` the TUI's
// vt100 parser drains every tick.  Webui SSH sessions don't have a TUI tab —
// the host owns the russh socket and the browser owns the xterm.js renderer.
//
// `spawn_remote_session` does NOT return sync channels for a TUI tab.
// Instead, every byte the russh driver delivers is base64-encoded and
// broadcast as a `RelayMessage::SshTerminalData { tab_id, data_b64, seq }`
// directly to the relay's broadcast sender.  The browser dispatches on
// `tab_id` against its per-tab xterm.js map.
//
// Returns a handle the caller stashes per `tab_id` so subsequent
// `ssh_terminal_input` / `ssh_terminal_resize` / `ssh_disconnect` messages
// can route to this session.

/// Handle to a remote SSH session bridged to the relay broadcast channel.
///
/// `stdin_tx` and `resize_tx` are sync senders the relay-input dispatcher
/// uses to forward viewer-side keystrokes and pane resizes.
///
/// Dropping the handle drops the senders, which causes the bridge thread's
/// tokio tasks to exit and the russh session to be closed.
pub struct RemoteSshHandle {
    pub stdin_tx: smpsc::Sender<Vec<u8>>,
    pub resize_tx: smpsc::Sender<(u32, u32)>,
}

/// Spawn an SSH session whose I/O fans out to the relay broadcast channel.
///
/// `tab_id` labels every emitted event so the browser can multiplex
/// concurrent SSH tabs on one WebSocket.  `relay_tx` is the host's
/// `RelayHost::event_sender()` — every chunk + every lifecycle transition
/// emits one `RelayMessage`.
///
/// Connection status events are emitted on the same broadcast channel as
/// `ssh_connection_status` so the browser can update its status footer
/// without a second subscription.
///
/// Per-tab sequence numbers start at 0 and monotonically advance for every
/// `ssh_terminal_data` event the host emits for this tab.  Gaps would
/// indicate a relay/transport drop (the relay uses a single in-order WS so
/// gaps are not expected, but the seq is the future-proof primitive for
/// flow-control / dedup).
pub fn spawn_remote_session(
    config: SshConfig,
    initial_size: (u32, u32),
    tab_id: usize,
    relay_tx: broadcast::Sender<RelayMessage>,
) -> RemoteSshHandle {
    let (stdin_tx, stdin_rx) = smpsc::channel::<Vec<u8>>();
    let (resize_tx, resize_rx) = smpsc::channel::<(u32, u32)>();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = relay_tx.send(RelayMessage::SshConnectionStatus {
                    tab_id,
                    status: "error".into(),
                    detail: format!("tokio runtime build failed: {e}"),
                });
                return;
            }
        };

        rt.block_on(async move {
            let _ = relay_tx.send(RelayMessage::SshConnectionStatus {
                tab_id,
                status: "connecting".into(),
                detail: String::new(),
            });

            let mut session = match connect(config, initial_size).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = relay_tx.send(RelayMessage::SshConnectionStatus {
                        tab_id,
                        status: "error".into(),
                        detail: e.to_string(),
                    });
                    return;
                }
            };

            // stdin pump (sync → async)
            let stdin_pump = {
                let session_stdin = session.stdin.clone();
                tokio::task::spawn_blocking(move || {
                    while let Ok(bytes) = stdin_rx.recv() {
                        if session_stdin.blocking_send(bytes).is_err() {
                            break;
                        }
                    }
                })
            };

            // resize pump (sync → async)
            let resize_pump = {
                let session_resize = session.resize.clone();
                tokio::task::spawn_blocking(move || {
                    while let Ok(size) = resize_rx.recv() {
                        if session_resize.blocking_send(size).is_err() {
                            break;
                        }
                    }
                })
            };

            // Lifecycle relay (async → broadcast)
            let relay_tx_events = relay_tx.clone();
            let events_relay = tokio::spawn(async move {
                while let Some(ev) = session.events.recv().await {
                    let (status, detail) = match ev {
                        SshEvent::AuthAttempt { method } => {
                            ("auth".to_string(), method.to_string())
                        }
                        SshEvent::AuthFailure(reason) => ("auth_failed".to_string(), reason),
                        SshEvent::Connected => ("connected".to_string(), String::new()),
                        SshEvent::Disconnected(reason) => {
                            ("disconnected".to_string(), reason.unwrap_or_default())
                        }
                        SshEvent::Error(msg) => ("error".to_string(), msg),
                        // Skipped: Connecting (we already emitted that),
                        // AuthSuccess (immediately followed by Connected),
                        // InteractivePrompt (no remote UI for it yet).
                        _ => continue,
                    };
                    if relay_tx_events
                        .send(RelayMessage::SshConnectionStatus {
                            tab_id,
                            status,
                            detail,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });

            // Stdout pump (async → broadcast) — drives the task to completion.
            let mut seq: u64 = 0;
            while let Some(chunk) = session.stdout.recv().await {
                let data_b64 = STANDARD_NO_PAD.encode(&chunk);
                let s = seq;
                seq = seq.saturating_add(1);
                if relay_tx
                    .send(RelayMessage::SshTerminalData {
                        tab_id,
                        data_b64,
                        seq: s,
                    })
                    .is_err()
                {
                    break;
                }
            }

            // stdout closed → tell viewers the tab is dead.  Best-effort;
            // events_relay may have already sent Disconnected.
            let _ = relay_tx.send(RelayMessage::SshConnectionStatus {
                tab_id,
                status: "disconnected".into(),
                detail: String::new(),
            });

            // Pumps exit naturally when their senders drop.
            let _ = stdin_pump.await;
            let _ = resize_pump.await;
            let _ = events_relay.await;
        });
    });

    RemoteSshHandle {
        stdin_tx,
        resize_tx,
    }
}
