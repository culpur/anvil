//! SSH-tab state and rendering helpers (T5-Ssh-D).
//!
//! A tab that holds an `SshTabState` is in "SSH mode": it renders a vt100
//! virtual screen (painted from bytes streamed back by `runtime::ssh`) and
//! forwards keypresses as raw bytes back to the remote shell. Chat fields
//! (`log`, `pending_text`, `input`, `branches`, etc.) are unused.
//!
//! The state itself only holds:
//!   - the vt100 parser (the screen state machine)
//!   - send/receive channels into the runtime SSH driver
//!   - a small connection-status string for the status line
//!
//! All channels are sync `mpsc` because the TUI loop is single-threaded.
//! The async russh driver runs on a tokio runtime in a separate thread
//! that bridges to these sync channels (see `tui::ssh_bridge`).

use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use vt100::Parser;

/// Connection lifecycle as the TUI knows it. Mirrors the relevant
/// `runtime::ssh::SshEvent` variants but trimmed to UI-visible states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshConnState {
    Connecting,
    AuthInProgress(String),     // method label
    Connected,
    /// Auth failed; tab is dead. Inner string is the reason.
    AuthFailed(String),
    /// Channel closed. Inner is reason if known.
    Disconnected(Option<String>),
    /// Local error before/after connect.
    Error(String),
}

impl SshConnState {
    /// Short status-line label.
    pub fn label(&self) -> String {
        match self {
            Self::Connecting => "connecting".to_string(),
            Self::AuthInProgress(m) => format!("auth ({m})"),
            Self::Connected => "connected".to_string(),
            Self::AuthFailed(_) => "auth failed".to_string(),
            Self::Disconnected(_) => "disconnected".to_string(),
            Self::Error(_) => "error".to_string(),
        }
    }
}

/// Per-tab SSH state. Owned by `Tab`.
pub struct SshTabState {
    /// vt100 virtual screen. Resized when the tab pane resizes.
    pub parser: Parser,
    /// Bytes from remote shell (stdout/stderr) to feed into `parser`.
    pub stdout_rx: Receiver<Vec<u8>>,
    /// Bytes typed by the user, sent to the remote shell.
    pub stdin_tx: Sender<Vec<u8>>,
    /// Resize events forwarded to the SSH driver as window-change requests.
    pub resize_tx: Sender<(u32, u32)>,
    /// Lifecycle event stream (auth attempt / connected / disconnected / …).
    pub events_rx: Receiver<crate::tui::ssh_bridge::UiSshEvent>,
    /// Current high-level connection state, for the status line.
    pub state: SshConnState,
    /// "user@host:port" for the status line and tab title.
    pub destination: String,
    /// When the connection was opened (or attempted), for "connected for Xs".
    pub opened_at: Instant,
    /// Set after `Disconnected` so we don't redraw forever on a dead tab.
    pub stream_finished: bool,
}

impl SshTabState {
    pub fn new(
        destination: String,
        cols: u16,
        rows: u16,
        stdout_rx: Receiver<Vec<u8>>,
        stdin_tx: Sender<Vec<u8>>,
        resize_tx: Sender<(u32, u32)>,
        events_rx: Receiver<crate::tui::ssh_bridge::UiSshEvent>,
    ) -> Self {
        Self {
            // vt100::Parser::new takes (rows, cols, scrollback_len).
            parser: Parser::new(rows, cols, 1000),
            stdout_rx,
            stdin_tx,
            resize_tx,
            events_rx,
            state: SshConnState::Connecting,
            destination,
            opened_at: Instant::now(),
            stream_finished: false,
        }
    }

    /// Drain pending stdout bytes into the vt100 parser. Returns true if
    /// any bytes were consumed (caller can decide to redraw).
    pub fn drain_stdout(&mut self) -> bool {
        let mut got = false;
        while let Ok(chunk) = self.stdout_rx.try_recv() {
            self.parser.process(&chunk);
            got = true;
        }
        got
    }

    /// Drain pending lifecycle events, updating `state`. Returns true if
    /// state changed.
    pub fn drain_events(&mut self) -> bool {
        use crate::tui::ssh_bridge::UiSshEvent as E;
        let mut changed = false;
        while let Ok(ev) = self.events_rx.try_recv() {
            changed = true;
            match ev {
                E::AuthAttempt(method) => {
                    self.state = SshConnState::AuthInProgress(method);
                }
                E::Connected => {
                    self.state = SshConnState::Connected;
                }
                E::AuthFailed(reason) => {
                    self.state = SshConnState::AuthFailed(reason);
                    self.stream_finished = true;
                }
                E::Disconnected(reason) => {
                    self.state = SshConnState::Disconnected(reason);
                    self.stream_finished = true;
                }
                E::Error(msg) => {
                    self.state = SshConnState::Error(msg);
                    self.stream_finished = true;
                }
            }
        }
        changed
    }

    /// Forward a typed key (already encoded as bytes) to the remote shell.
    /// Silently ignored if the channel has dropped.
    pub fn send_bytes(&self, bytes: &[u8]) {
        let _ = self.stdin_tx.send(bytes.to_vec());
    }

    /// Forward a resize event. Also resizes the vt100 parser so locally
    /// rendered text wraps to the new geometry.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.parser.screen_mut().set_size(rows, cols);
        let _ = self.resize_tx.send((u32::from(cols), u32::from(rows)));
    }
}

/// Encode a single keypress (crossterm KeyEvent) into the byte sequence that
/// xterm/POSIX line-disciplined shells expect over an SSH PTY. Covers the
/// keys that matter for typical shell + vim use; unrecognised keys map to
/// nothing (silently dropped).
///
/// Public so `tui::input_handler` can call it directly when the active tab
/// is in SSH mode.
pub fn key_event_to_bytes(key: crossterm::event::KeyEvent) -> Vec<u8> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let mods = key.modifiers;
    match key.code {
        KeyCode::Char(c) => {
            // Ctrl+letter → control byte 0x01–0x1A.
            if mods.contains(KeyModifiers::CONTROL) && c.is_ascii_alphabetic() {
                let b = c.to_ascii_lowercase() as u8 - b'a' + 1;
                vec![b]
            } else if mods.contains(KeyModifiers::ALT) {
                // Alt-prefixed: ESC + char (xterm convention).
                let mut buf = vec![0x1b];
                buf.extend(c.to_string().into_bytes());
                buf
            } else {
                c.to_string().into_bytes()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => vec![0x1b, b'[', b'Z'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => vec![0x1b, b'[', b'A'],
        KeyCode::Down => vec![0x1b, b'[', b'B'],
        KeyCode::Right => vec![0x1b, b'[', b'C'],
        KeyCode::Left => vec![0x1b, b'[', b'D'],
        KeyCode::Home => vec![0x1b, b'[', b'H'],
        KeyCode::End => vec![0x1b, b'[', b'F'],
        KeyCode::PageUp => vec![0x1b, b'[', b'5', b'~'],
        KeyCode::PageDown => vec![0x1b, b'[', b'6', b'~'],
        KeyCode::Delete => vec![0x1b, b'[', b'3', b'~'],
        KeyCode::Insert => vec![0x1b, b'[', b'2', b'~'],
        KeyCode::F(n) => match n {
            1 => vec![0x1b, b'O', b'P'],
            2 => vec![0x1b, b'O', b'Q'],
            3 => vec![0x1b, b'O', b'R'],
            4 => vec![0x1b, b'O', b'S'],
            5 => vec![0x1b, b'[', b'1', b'5', b'~'],
            6 => vec![0x1b, b'[', b'1', b'7', b'~'],
            7 => vec![0x1b, b'[', b'1', b'8', b'~'],
            8 => vec![0x1b, b'[', b'1', b'9', b'~'],
            9 => vec![0x1b, b'[', b'2', b'0', b'~'],
            10 => vec![0x1b, b'[', b'2', b'1', b'~'],
            11 => vec![0x1b, b'[', b'2', b'3', b'~'],
            12 => vec![0x1b, b'[', b'2', b'4', b'~'],
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn kc(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn plain_char_encodes_to_single_byte() {
        assert_eq!(key_event_to_bytes(k(KeyCode::Char('a'))), b"a".to_vec());
    }

    #[test]
    fn ctrl_c_is_etx() {
        assert_eq!(
            key_event_to_bytes(kc(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![0x03],
        );
    }

    #[test]
    fn arrow_keys_use_csi_letters() {
        assert_eq!(key_event_to_bytes(k(KeyCode::Up)), vec![0x1b, b'[', b'A']);
        assert_eq!(key_event_to_bytes(k(KeyCode::Right)), vec![0x1b, b'[', b'C']);
    }

    #[test]
    fn enter_is_carriage_return() {
        assert_eq!(key_event_to_bytes(k(KeyCode::Enter)), b"\r".to_vec());
    }

    #[test]
    fn backspace_is_del() {
        assert_eq!(key_event_to_bytes(k(KeyCode::Backspace)), vec![0x7f]);
    }

    #[test]
    fn alt_a_is_esc_a() {
        assert_eq!(
            key_event_to_bytes(kc(KeyCode::Char('a'), KeyModifiers::ALT)),
            vec![0x1b, b'a'],
        );
    }

    #[test]
    fn f1_through_f12_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for n in 1..=12u8 {
            let bytes = key_event_to_bytes(k(KeyCode::F(n)));
            assert!(!bytes.is_empty(), "F{n} produced empty");
            assert!(seen.insert(bytes), "F{n} duplicate encoding");
        }
    }

    #[test]
    fn conn_state_label_is_human_readable() {
        assert_eq!(SshConnState::Connecting.label(), "connecting");
        assert_eq!(
            SshConnState::AuthInProgress("password".into()).label(),
            "auth (password)"
        );
        assert_eq!(SshConnState::Connected.label(), "connected");
    }
}
