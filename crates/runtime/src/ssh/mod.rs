//! Embedded SSH client for Anvil.
//!
//! Phase B ships the pure connection layer: russh driver, 4-method auth chain,
//! and bidirectional I/O channels. TUI integration follows in Phase D.

#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc, clippy::missing_panics_doc)]

mod config;
mod driver;
mod session;

#[cfg(test)]
mod tests;

pub use config::{SshAuthMethod, SshConfig};
pub use session::{SshError, SshEvent, SshSession};

/// Spawn an SSH session.
///
/// Returns once the TCP+SSH handshake completes and authentication has been
/// **attempted** (the result is delivered via [`SshSession::events`]).
///
/// Returns an `Err` only for local setup failures (DNS, file I/O, missing
/// env var). Auth rejections surface through [`SshEvent::AuthFailure`] so
/// the caller can retry without re-creating the task.
pub async fn connect(
    config: SshConfig,
    initial_size: (u32, u32),
) -> Result<SshSession, SshError> {
    driver::connect_and_spawn(config, initial_size).await
}
