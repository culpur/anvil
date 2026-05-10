//! russh client handler, auth loop, and I/O pump tasks.

#![allow(clippy::module_name_repetitions)]

use std::sync::Arc;

use async_trait::async_trait;
use russh::client::{self, KeyboardInteractiveAuthResponse};
use russh::keys::key::PrivateKeyWithHashAlg;
use russh::{Channel, ChannelMsg};
use tokio::sync::{mpsc, oneshot};

use crate::ssh::config::{SshAuthMethod, SshConfig};
use crate::ssh::session::{SshError, SshEvent, SshSession};

/// Channel buffer sizes for stdin/stdout/events/resize.
const CHAN_BUF: usize = 256;

/// The russh `Handler` implementation for Anvil's SSH client.
pub(crate) struct ClientHandler;

#[async_trait]
impl client::Handler for ClientHandler {
    type Error = russh::Error;

    // TODO(v2.3): record server key fingerprint to ~/.anvil/known_hosts
    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Build the SSH connection, authenticate, open a shell, and return a live
/// `SshSession`. Events are reported via `event_tx` which is consumed by the
/// caller into the `SshSession::events` receiver.
async fn run_inner(
    config: SshConfig,
    initial_size: (u32, u32),
    event_tx: mpsc::Sender<SshEvent>,
) -> Result<SshSession, SshError> {
    let _ = event_tx.send(SshEvent::Connecting).await;

    // --- TCP connect + SSH handshake ---
    let russh_config = Arc::new(client::Config::default());
    let addr = format!("{}:{}", config.host, config.port);
    let mut session = client::connect(russh_config, addr, ClientHandler)
        .await
        .map_err(|e| SshError::Connect(e.to_string()))?;

    // --- authenticate ---
    let authed = do_auth(&mut session, &config, &event_tx).await?;
    if !authed {
        // AuthFailure event already sent by do_auth.
        return Err(SshError::Auth("authentication rejected".into()));
    }

    // --- open channel + PTY + shell ---
    let mut channel: Channel<client::Msg> = session
        .channel_open_session()
        .await
        .map_err(|e| SshError::Channel(e.to_string()))?;

    let (cols, rows) = initial_size;
    channel
        .request_pty(true, "xterm-256color", cols, rows, 0, 0, &[])
        .await
        .map_err(|e| SshError::Channel(e.to_string()))?;

    channel
        .request_shell(true)
        .await
        .map_err(|e| SshError::Channel(e.to_string()))?;

    let _ = event_tx.send(SshEvent::Connected).await;

    // --- build channel handles ---
    let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>(CHAN_BUF);
    let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>(CHAN_BUF);
    let (resize_tx, resize_rx) = mpsc::channel::<(u32, u32)>(CHAN_BUF);

    // stdin pump: bytes typed in UI → remote shell stdin
    let stdin_writer = channel.make_writer();
    let stdin_event_tx = event_tx.clone();
    tokio::spawn(async move {
        run_stdin_pump(stdin_rx, stdin_writer, stdin_event_tx).await;
    });

    // stdout + resize pump: remote output → UI, resize window-change → wire
    let stdout_event_tx = event_tx.clone();
    tokio::spawn(async move {
        run_output_resize_pump(channel, stdout_tx, resize_rx, stdout_event_tx).await;
    });

    // Return a partial SshSession; the real `events` receiver is spliced in by
    // the public `connect_and_spawn` wrapper below.
    Ok(SshSession {
        stdin: stdin_tx,
        stdout: stdout_rx,
        events: mpsc::channel::<SshEvent>(1).1, // placeholder — replaced by caller
        resize: resize_tx,
    })
}

/// Public entry point. Creates the event channel, runs `run_inner`, and
/// stitches the event receiver back into the returned `SshSession`.
pub(crate) async fn connect_and_spawn(
    config: SshConfig,
    initial_size: (u32, u32),
) -> Result<SshSession, SshError> {
    let (event_tx, event_rx) = mpsc::channel::<SshEvent>(CHAN_BUF);
    let mut session = run_inner(config, initial_size, event_tx).await?;
    session.events = event_rx;
    Ok(session)
}

// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

async fn do_auth(
    session: &mut client::Handle<ClientHandler>,
    config: &SshConfig,
    event_tx: &mpsc::Sender<SshEvent>,
) -> Result<bool, SshError> {
    match &config.auth {
        SshAuthMethod::Agent => auth_agent(session, &config.user, event_tx).await,
        SshAuthMethod::KeyFile { path, passphrase } => {
            auth_key_file(session, &config.user, path, passphrase.as_deref(), event_tx).await
        }
        SshAuthMethod::Password(secret) => {
            auth_password(session, &config.user, secret, event_tx).await
        }
        SshAuthMethod::KeyboardInteractive => {
            auth_keyboard_interactive(session, &config.user, event_tx).await
        }
    }
}

async fn auth_agent(
    session: &mut client::Handle<ClientHandler>,
    user: &str,
    event_tx: &mpsc::Sender<SshEvent>,
) -> Result<bool, SshError> {
    let sock = match std::env::var("SSH_AUTH_SOCK") {
        Ok(p) => p,
        Err(_) => {
            let _ = event_tx
                .send(SshEvent::AuthFailure(
                    "no SSH agent socket (SSH_AUTH_SOCK unset)".into(),
                ))
                .await;
            return Ok(false);
        }
    };

    let _ = event_tx
        .send(SshEvent::AuthAttempt { method: "agent" })
        .await;

    let mut agent = russh::keys::agent::client::AgentClient::connect_uds(&sock)
        .await
        .map_err(|e| SshError::Auth(e.to_string()))?;

    let identities = agent
        .request_identities()
        .await
        .map_err(|e| SshError::Auth(e.to_string()))?;

    if identities.is_empty() {
        let _ = event_tx
            .send(SshEvent::AuthFailure("SSH agent has no keys loaded".into()))
            .await;
        return Ok(false);
    }

    for pubkey in identities {
        let ok = session
            .authenticate_publickey_with(user, pubkey, &mut agent)
            .await
            .map_err(|e| SshError::Auth(e.to_string()))?;
        if ok {
            let _ = event_tx.send(SshEvent::AuthSuccess).await;
            return Ok(true);
        }
    }

    let _ = event_tx
        .send(SshEvent::AuthFailure(
            "agent: all keys rejected by server".into(),
        ))
        .await;
    Ok(false)
}

async fn auth_key_file(
    session: &mut client::Handle<ClientHandler>,
    user: &str,
    path: &std::path::Path,
    passphrase: Option<&str>,
    event_tx: &mpsc::Sender<SshEvent>,
) -> Result<bool, SshError> {
    let _ = event_tx
        .send(SshEvent::AuthAttempt { method: "key" })
        .await;

    let private_key = russh::keys::load_secret_key(path, passphrase)
        .map_err(|e| SshError::Auth(format!("cannot load key {}: {e}", path.display())))?;

    let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(private_key), None)
        .map_err(|e| SshError::Auth(e.to_string()))?;

    let ok = session
        .authenticate_publickey(user, key_with_hash)
        .await
        .map_err(|e| SshError::Auth(e.to_string()))?;

    if ok {
        let _ = event_tx.send(SshEvent::AuthSuccess).await;
        Ok(true)
    } else {
        let _ = event_tx
            .send(SshEvent::AuthFailure("key rejected by server".into()))
            .await;
        Ok(false)
    }
}

async fn auth_password(
    session: &mut client::Handle<ClientHandler>,
    user: &str,
    password: &str,
    event_tx: &mpsc::Sender<SshEvent>,
) -> Result<bool, SshError> {
    let _ = event_tx
        .send(SshEvent::AuthAttempt { method: "password" })
        .await;

    let ok = session
        .authenticate_password(user, password)
        .await
        .map_err(|e| SshError::Auth(e.to_string()))?;

    if ok {
        let _ = event_tx.send(SshEvent::AuthSuccess).await;
        Ok(true)
    } else {
        let _ = event_tx
            .send(SshEvent::AuthFailure("password rejected by server".into()))
            .await;
        Ok(false)
    }
}

async fn auth_keyboard_interactive(
    session: &mut client::Handle<ClientHandler>,
    user: &str,
    event_tx: &mpsc::Sender<SshEvent>,
) -> Result<bool, SshError> {
    let _ = event_tx
        .send(SshEvent::AuthAttempt { method: "interactive" })
        .await;

    let mut resp = session
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(|e| SshError::Auth(e.to_string()))?;

    loop {
        match resp {
            KeyboardInteractiveAuthResponse::Success => {
                let _ = event_tx.send(SshEvent::AuthSuccess).await;
                return Ok(true);
            }
            KeyboardInteractiveAuthResponse::Failure => {
                let _ = event_tx
                    .send(SshEvent::AuthFailure(
                        "keyboard-interactive: server rejected".into(),
                    ))
                    .await;
                return Ok(false);
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let prompt_pairs: Vec<(String, bool)> = prompts
                    .iter()
                    .map(|p| (p.prompt.clone(), p.echo))
                    .collect();

                let (answer_tx, answer_rx) = oneshot::channel::<Vec<String>>();
                let _ = event_tx
                    .send(SshEvent::InteractivePrompt {
                        name,
                        instructions,
                        prompts: prompt_pairs,
                        respond: answer_tx,
                    })
                    .await;

                let answers = answer_rx.await.unwrap_or_default();
                resp = session
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(|e| SshError::Auth(e.to_string()))?;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// I/O pumps
// ---------------------------------------------------------------------------

async fn run_stdin_pump(
    mut stdin_rx: mpsc::Receiver<Vec<u8>>,
    mut writer: impl tokio::io::AsyncWrite + Unpin,
    event_tx: mpsc::Sender<SshEvent>,
) {
    use tokio::io::AsyncWriteExt;
    while let Some(data) = stdin_rx.recv().await {
        if writer.write_all(&data).await.is_err() {
            let _ = event_tx
                .send(SshEvent::Error("stdin write failed".into()))
                .await;
            return;
        }
    }
}

async fn run_output_resize_pump(
    mut channel: Channel<client::Msg>,
    stdout_tx: mpsc::Sender<Vec<u8>>,
    mut resize_rx: mpsc::Receiver<(u32, u32)>,
    event_tx: mpsc::Sender<SshEvent>,
) {
    loop {
        tokio::select! {
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        if stdout_tx.send(data.to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        if stdout_tx.send(data.to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Some(ChannelMsg::Eof) | None => {
                        let _ = event_tx.send(SshEvent::Disconnected(None)).await;
                        return;
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        let reason = if exit_status == 0 {
                            None
                        } else {
                            Some(format!("exit status {exit_status}"))
                        };
                        let _ = event_tx.send(SshEvent::Disconnected(reason)).await;
                        return;
                    }
                    Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                        let reason = format!("killed by signal {signal_name:?}");
                        let _ = event_tx
                            .send(SshEvent::Disconnected(Some(reason)))
                            .await;
                        return;
                    }
                    _ => {}
                }
            }
            size = resize_rx.recv() => {
                match size {
                    Some((cols, rows)) => {
                        let _ = channel.window_change(cols, rows, 0, 0).await;
                    }
                    None => {
                        // Resize sender dropped — not fatal, keep running.
                    }
                }
            }
        }
    }

    let _ = event_tx.send(SshEvent::Disconnected(None)).await;
}
