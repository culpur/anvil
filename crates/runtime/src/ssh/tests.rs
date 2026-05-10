//! In-process russh server tests for the SSH driver.
//!
//! Each test spins up a real TCP listener on 127.0.0.1:0, accepts one
//! connection via `russh::server::run_stream`, and exercises the public
//! `ssh::connect` interface.  All recvs are guarded by a 5-second timeout so
//! the suite is CI-safe even on slow machines.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc
)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use russh::server::{self, Auth, Msg, Session};
use russh::{Channel, ChannelId, CryptoVec};
// russh_keys re-exports PrivateKey, PublicKey, and the ssh_key sub-crate.
use russh_keys::{ssh_key, PrivateKey};
// rand 0.8 OsRng implements CryptoRng + RngCore, compatible with ssh_key 0.6.
use rand::rngs::OsRng;
use tokio::sync::{mpsc, Mutex};

use crate::ssh::config::{SshAuthMethod, SshConfig};
use crate::ssh::session::SshEvent;

// ---------------------------------------------------------------------------
// Shared test infrastructure
// ---------------------------------------------------------------------------

/// The ed25519 key pair used by all key-auth tests.  Generated once per
/// process and reused across tests for speed.
fn test_client_keypair() -> PrivateKey {
    PrivateKey::random(&mut OsRng, ssh_key::Algorithm::Ed25519).unwrap()
}

/// Notification sent by the test server handler to the test body.
#[derive(Debug)]
enum ServerEvent {
    ShellOpened(ChannelId),
    DataReceived(Vec<u8>),
    WindowChanged(u32, u32),
}

/// Per-connection state shared between the server handler and the test body.
struct TestHandlerState {
    /// Accept this password ("test-pass").
    accepted_password: &'static str,
    /// Accept this public key (checked by fingerprint).
    accepted_pubkey_fp: Option<String>,
    /// Notify the test body of interesting events.
    event_tx: mpsc::Sender<ServerEvent>,
    /// Track the last received terminal size for resize tests.
    last_size: Arc<Mutex<(u32, u32)>>,
}

#[async_trait]
impl server::Handler for TestHandlerState {
    type Error = russh::Error;

    // ---- auth ----

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        if password == self.accepted_password {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::Reject {
                proceed_with_methods: None,
            })
        }
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        public_key: &ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        match &self.accepted_pubkey_fp {
            Some(fp) if *fp == public_key.fingerprint(Default::default()).to_string() => {
                Ok(Auth::Accept)
            }
            _ => Ok(Auth::Reject {
                proceed_with_methods: None,
            }),
        }
    }

    // ---- channel ----

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        let _ = self
            .event_tx
            .send(ServerEvent::ShellOpened(channel.id()))
            .await;
        // Spawn a task that echoes received data back to the client.
        let id = channel.id();
        let event_tx = self.event_tx.clone();
        let last_size = self.last_size.clone();
        tokio::spawn(async move {
            channel_pump(channel, id, event_tx, last_size).await;
        });
        Ok(true)
    }

    async fn pty_request(
        &mut self,
        _channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(_channel);
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Emit "READY\n" so tests have a stable sentinel to wait for.
        session.data(channel, CryptoVec::from_slice(b"READY\n"))?;
        session.channel_success(channel);
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        *self.last_size.lock().await = (col_width, row_height);
        let _ = self
            .event_tx
            .send(ServerEvent::WindowChanged(col_width, row_height))
            .await;
        session.channel_success(_channel);
        Ok(())
    }
}

/// Drive the server-side channel: echo data back to the client and forward
/// important events to the test body.
async fn channel_pump(
    mut ch: Channel<Msg>,
    id: ChannelId,
    event_tx: mpsc::Sender<ServerEvent>,
    last_size: Arc<Mutex<(u32, u32)>>,
) {
    use russh::ChannelMsg;
    while let Some(msg) = ch.wait().await {
        match msg {
            ChannelMsg::Data { data } => {
                let bytes = data.to_vec();
                let _ = event_tx
                    .send(ServerEvent::DataReceived(bytes.clone()))
                    .await;
                // Echo the data back.
                let _ = ch.data(&bytes[..]).await;
            }
            ChannelMsg::WindowChange {
                col_width,
                row_height,
                ..
            } => {
                *last_size.lock().await = (col_width, row_height);
                let _ = event_tx
                    .send(ServerEvent::WindowChanged(col_width, row_height))
                    .await;
            }
            _ => {}
        }
        // suppress unused variable warnings
        let _ = (id, &last_size);
    }
}

/// Spawn an in-process server configured as requested, return the bound port.
async fn spawn_server(
    accepted_pubkey_fp: Option<String>,
    server_event_tx: mpsc::Sender<ServerEvent>,
    last_size: Arc<Mutex<(u32, u32)>>,
) -> u16 {
    let server_key =
        PrivateKey::random(&mut OsRng, ssh_key::Algorithm::Ed25519).unwrap();
    let config = Arc::new(russh::server::Config {
        keys: vec![server_key],
        // Speed up auth rejection for wrong-password test.
        auth_rejection_time: Duration::from_millis(10),
        auth_rejection_time_initial: Some(Duration::from_millis(0)),
        inactivity_timeout: None,
        ..Default::default()
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        let (socket, _peer) = listener.accept().await.unwrap();
        let handler = TestHandlerState {
            accepted_password: "test-pass",
            accepted_pubkey_fp,
            event_tx: server_event_tx,
            last_size,
        };
        let running = russh::server::run_stream(config, socket, handler)
            .await
            .unwrap();
        // Drive the session to completion (or just let it go in the background).
        drop(running);
    });

    port
}

/// Convenience: drain the client's event channel until we find `SshEvent::Connected`,
/// returning an error string if auth failed first.
async fn wait_connected(events: &mut mpsc::Receiver<SshEvent>) -> Result<(), String> {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .map_err(|_| "timeout waiting for SshEvent".to_string())?
            .ok_or_else(|| "event channel closed".to_string())?;
        match ev {
            SshEvent::Connected => return Ok(()),
            SshEvent::AuthFailure(reason) => return Err(reason),
            SshEvent::Error(e) => return Err(e),
            _ => {}
        }
    }
}

/// Convenience: receive from `stdout` with a 5-second deadline.
async fn recv_stdout(stdout: &mut mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(5), stdout.recv())
        .await
        .expect("timeout waiting for stdout")
        .expect("stdout channel closed")
}

// ---------------------------------------------------------------------------
// Test 1: password auth succeeds and streams data
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn connect_with_password_succeeds_and_streams_data() {
    let last_size = Arc::new(Mutex::new((0u32, 0u32)));
    let (srv_tx, _srv_rx) = mpsc::channel::<ServerEvent>(32);
    let port = spawn_server(None, srv_tx, last_size).await;

    let config = SshConfig {
        host: "127.0.0.1".into(),
        port,
        user: "testuser".into(),
        auth: SshAuthMethod::Password("test-pass".into()),
    };
    let mut session = crate::ssh::connect(config, (80, 24)).await.unwrap();

    // Should reach Connected.
    wait_connected(&mut session.events)
        .await
        .expect("password auth should succeed");

    // We should receive "READY\n" from shell_request.
    let data = recv_stdout(&mut session.stdout).await;
    assert!(
        data.windows(6).any(|w| w == b"READY\n"),
        "expected READY\\n in stdout, got: {:?}",
        String::from_utf8_lossy(&data)
    );
}

// ---------------------------------------------------------------------------
// Test 2: wrong password emits AuthFailure
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn connect_with_wrong_password_emits_auth_failure() {
    let last_size = Arc::new(Mutex::new((0u32, 0u32)));
    let (srv_tx, _srv_rx) = mpsc::channel::<ServerEvent>(32);
    let port = spawn_server(None, srv_tx, last_size).await;

    let config = SshConfig {
        host: "127.0.0.1".into(),
        port,
        user: "testuser".into(),
        auth: SshAuthMethod::Password("wrong-password".into()),
    };

    // connect() returns Err when auth is rejected (driver converts AuthFailure → Err).
    let result = crate::ssh::connect(config, (80, 24)).await;
    assert!(
        result.is_err(),
        "wrong password should return Err from connect()"
    );
}

// ---------------------------------------------------------------------------
// Test 3: key-file auth succeeds
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn connect_with_key_file_succeeds() {
    let client_key = test_client_keypair();
    let fp = client_key
        .public_key()
        .fingerprint(Default::default())
        .to_string();

    // Write key to a temp file in OpenSSH PEM format.
    let key_path = {
        let mut p = std::env::temp_dir();
        p.push(format!("anvil-test-key-{}.pem", std::process::id()));
        p
    };
    client_key
        .write_openssh_file(&key_path, ssh_key::LineEnding::LF)
        .unwrap();

    let last_size = Arc::new(Mutex::new((0u32, 0u32)));
    let (srv_tx, _srv_rx) = mpsc::channel::<ServerEvent>(32);
    let port = spawn_server(Some(fp), srv_tx, last_size).await;

    let config = SshConfig {
        host: "127.0.0.1".into(),
        port,
        user: "testuser".into(),
        auth: SshAuthMethod::KeyFile {
            path: key_path.clone(),
            passphrase: None,
        },
    };

    let mut session = crate::ssh::connect(config, (80, 24)).await.unwrap();
    wait_connected(&mut session.events)
        .await
        .expect("key-file auth should succeed");

    // Cleanup.
    let _ = std::fs::remove_file(&key_path);
}

// ---------------------------------------------------------------------------
// Test 4: stdin writes reach the shell and echo back
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn stdin_writes_reach_shell_and_echo_back() {
    let last_size = Arc::new(Mutex::new((0u32, 0u32)));
    let (srv_tx, _srv_rx) = mpsc::channel::<ServerEvent>(32);
    let port = spawn_server(None, srv_tx, last_size).await;

    let config = SshConfig {
        host: "127.0.0.1".into(),
        port,
        user: "testuser".into(),
        auth: SshAuthMethod::Password("test-pass".into()),
    };
    let mut session = crate::ssh::connect(config, (80, 24)).await.unwrap();
    wait_connected(&mut session.events)
        .await
        .unwrap();

    // Drain the READY\n banner first (may be split across chunks).
    let _ = recv_stdout(&mut session.stdout).await;

    // Send a payload and expect it to echo back.
    let payload = b"hello from client\n";
    session.stdin.send(payload.to_vec()).await.unwrap();

    // Collect chunks until we see our payload echoed back.
    let mut received = Vec::new();
    loop {
        let chunk = recv_stdout(&mut session.stdout).await;
        received.extend_from_slice(&chunk);
        if received
            .windows(payload.len())
            .any(|w| w == payload)
        {
            break;
        }
        // If we have collected more than 4 KB without a match, something is wrong.
        if received.len() > 4096 {
            panic!(
                "did not see echo within 4 KB, got: {:?}",
                String::from_utf8_lossy(&received)
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Test 5: resize request is forwarded to the server
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn resize_request_is_forwarded() {
    let last_size = Arc::new(Mutex::new((0u32, 0u32)));
    let last_size_check = last_size.clone();

    let (srv_tx, mut srv_rx) = mpsc::channel::<ServerEvent>(32);
    let port = spawn_server(None, srv_tx, last_size).await;

    let config = SshConfig {
        host: "127.0.0.1".into(),
        port,
        user: "testuser".into(),
        auth: SshAuthMethod::Password("test-pass".into()),
    };
    let mut session = crate::ssh::connect(config, (80, 24)).await.unwrap();
    wait_connected(&mut session.events).await.unwrap();

    // Drain the ShellOpened server event so the channel pump is running.
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), srv_rx.recv())
            .await
            .expect("timeout waiting for ShellOpened")
            .expect("server event channel closed");
        if matches!(ev, ServerEvent::ShellOpened(_)) {
            break;
        }
    }

    // Send a resize.
    session.resize.send((132, 50)).await.unwrap();

    // Wait for the server to record the new size.
    let deadline = Duration::from_secs(5);
    let check = tokio::time::timeout(deadline, async {
        loop {
            let size = *last_size_check.lock().await;
            if size == (132, 50) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;

    assert!(
        check.is_ok(),
        "server did not record new terminal size (132, 50) within 5 seconds"
    );
}
