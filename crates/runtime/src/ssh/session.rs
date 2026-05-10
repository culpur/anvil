//! Public session handle types and lifecycle events.

use tokio::sync::{mpsc, oneshot};

/// Handle returned by [`super::connect`]. All I/O with the remote shell goes
/// through the four channels in this struct.
pub struct SshSession {
    /// Write raw bytes to the remote shell stdin (keyboard input, etc.).
    pub stdin: mpsc::Sender<Vec<u8>>,
    /// Receive raw bytes from the remote shell stdout/stderr combined stream.
    pub stdout: mpsc::Receiver<Vec<u8>>,
    /// Receive lifecycle and auth events from the driver task.
    pub events: mpsc::Receiver<SshEvent>,
    /// Notify the driver of a terminal resize. Send `(cols, rows)`.
    pub resize: mpsc::Sender<(u32, u32)>,
}

/// Events emitted by the SSH driver task over `SshSession::events`.
pub enum SshEvent {
    /// TCP connection attempt in progress.
    Connecting,
    /// An authentication attempt is starting.
    AuthAttempt {
        /// One of `"agent"`, `"key"`, `"password"`, or `"interactive"`.
        method: &'static str,
    },
    /// The server accepted the credentials.
    AuthSuccess,
    /// The server rejected the credentials. Contains a human-readable reason.
    AuthFailure(String),
    /// The server is requesting interactive authentication prompts.
    ///
    /// Display each prompt string to the user (respecting `echo`), collect
    /// answers, and send them back via `respond`. The `Vec<String>` must have
    /// the same length as `prompts`.
    InteractivePrompt {
        name: String,
        instructions: String,
        prompts: Vec<(String, bool)>,
        respond: oneshot::Sender<Vec<String>>,
    },
    /// PTY allocated and shell channel open — the session is ready.
    Connected,
    /// The session ended. `None` means a clean server-side close; `Some` carries
    /// the disconnect reason text when available.
    Disconnected(Option<String>),
    /// An unrecoverable error occurred before or after `Connected`.
    Error(String),
}

/// Errors that can be returned by [`super::connect`].
///
/// Auth failures are **not** returned here; they surface through
/// [`SshEvent::AuthFailure`] so the UI can drive a retry without
/// recreating the task.
#[derive(Debug)]
pub enum SshError {
    /// A local I/O problem (DNS, file system, etc.).
    Io(std::io::Error),
    /// TCP connect or SSH handshake failed.
    Connect(String),
    /// A local auth setup error (e.g. cannot read key file).
    Auth(String),
    /// A channel-level error (PTY request, shell open).
    Channel(String),
}

impl std::fmt::Display for SshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SshError::Io(e) => write!(f, "I/O error: {e}"),
            SshError::Connect(s) => write!(f, "Connect error: {s}"),
            SshError::Auth(s) => write!(f, "Auth error: {s}"),
            SshError::Channel(s) => write!(f, "Channel error: {s}"),
        }
    }
}

impl std::error::Error for SshError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SshError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SshError {
    fn from(e: std::io::Error) -> Self {
        SshError::Io(e)
    }
}
