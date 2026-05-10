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

use runtime::ssh::{connect, SshConfig, SshEvent};

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
