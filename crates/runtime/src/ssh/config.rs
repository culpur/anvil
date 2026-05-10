//! SSH connection configuration types.

use std::path::PathBuf;

/// Complete configuration for a single SSH connection.
#[derive(Debug, Clone)]
pub struct SshConfig {
    /// Hostname or IP address of the remote server.
    pub host: String,
    /// TCP port to connect to. The conventional default is 22.
    pub port: u16,
    /// Remote user to authenticate as.
    pub user: String,
    /// Authentication method to use. Only the selected method is tried;
    /// there is no silent fallthrough to a secondary method.
    pub auth: SshAuthMethod,
}

/// The authentication method Anvil should use for this connection.
#[derive(Debug, Clone)]
pub enum SshAuthMethod {
    /// Forward a request to the running SSH agent (`SSH_AUTH_SOCK`).
    ///
    /// Errors immediately with [`SshEvent::AuthFailure`] if the environment
    /// variable is not set. When the variable is set, every key the agent
    /// advertises is tried in order before giving up.
    Agent,
    /// Authenticate with a private key loaded from a PEM file on disk.
    ///
    /// `passphrase` decrypts the key if it is passphrase-protected;
    /// pass `None` for unencrypted keys.
    KeyFile {
        path: PathBuf,
        passphrase: Option<String>,
    },
    /// Authenticate with a plain password.
    Password(String),
    /// Authenticate via the SSH keyboard-interactive exchange (e.g. OTP / 2FA).
    ///
    /// Each server challenge round is surfaced as an [`SshEvent::InteractivePrompt`]
    /// event. The caller fills in answers via the bundled `oneshot` sender; the
    /// driver then forwards those answers to the server.
    KeyboardInteractive,
}
