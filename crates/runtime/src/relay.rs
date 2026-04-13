//! Relay module for Anvil Remote Control.
//!
//! Provides WebSocket-based session relay through Passage (`api.culpur.net`).
//! The CLI acts as a "host" and web browsers connect as "clients" — both
//! connect outbound to Passage which bridges them after pairing verification.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::rngs::OsRng;
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, Mutex};

// ─── Session Hash & Pairing Code Generation ─────────────────────────────────

/// Generate a cryptographically random session hash (256-bit → 43-char base64url).
pub fn generate_session_hash() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Generate a 6-digit pairing code.
pub fn generate_pairing_code() -> String {
    let code: u32 = OsRng.gen_range(0..1_000_000);
    format!("{code:06}")
}

// ─── Pairing Verifier ───────────────────────────────────────────────────────

/// Tracks a pairing code for a single client connection with expiry and attempt limits.
#[derive(Debug)]
pub struct PairingVerifier {
    code: String,
    attempts: u32,
    max_attempts: u32,
    expires_at: Instant,
}

impl PairingVerifier {
    /// Create a new verifier with a fresh code. Expires after `ttl`.
    pub fn new(code: String, ttl: Duration) -> Self {
        Self {
            code,
            attempts: 0,
            max_attempts: 3,
            expires_at: Instant::now() + ttl,
        }
    }

    /// Default TTL of 5 minutes.
    pub fn with_defaults(code: String) -> Self {
        Self::new(code, Duration::from_secs(300))
    }

    /// Verify a pairing attempt. Returns `Ok(())` on success.
    pub fn verify(&mut self, attempt: &str) -> Result<(), PairingError> {
        if Instant::now() > self.expires_at {
            return Err(PairingError::Expired);
        }
        if self.attempts >= self.max_attempts {
            return Err(PairingError::TooManyAttempts);
        }
        self.attempts += 1;
        if attempt == self.code {
            Ok(())
        } else {
            Err(PairingError::WrongCode {
                remaining: self.max_attempts - self.attempts,
            })
        }
    }

    /// The pairing code (for display in the TUI).
    pub fn code(&self) -> &str {
        &self.code
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingError {
    WrongCode { remaining: u32 },
    TooManyAttempts,
    Expired,
}

impl std::fmt::Display for PairingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongCode { remaining } => write!(f, "Wrong code ({remaining} attempts left)"),
            Self::TooManyAttempts => write!(f, "Too many failed attempts"),
            Self::Expired => write!(f, "Pairing code expired"),
        }
    }
}

impl std::error::Error for PairingError {}

// ─── Relay Protocol Messages ────────────────────────────────────────────────

/// All messages exchanged over the relay WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayMessage {
    // ── Connection setup ──
    HostHello {
        hash: String,
        protocol_version: u32,
    },
    ClientHello {
        hash: String,
    },
    ClientConnected {
        client_id: String,
    },
    PairingRequired,
    PairingAttempt {
        client_id: String,
        code: String,
    },
    PairingResult {
        client_id: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    // ── Session data ──
    SessionSnapshot {
        tabs: Vec<TabSnapshot>,
    },
    TextDelta {
        tab_id: usize,
        text: String,
    },
    TextDone {
        tab_id: usize,
    },
    ToolStart {
        tab_id: usize,
        name: String,
        detail: String,
    },
    ToolResult {
        tab_id: usize,
        name: String,
        summary: String,
        is_error: bool,
    },
    ThinkLabel {
        tab_id: usize,
        label: String,
    },
    TurnDone {
        tab_id: usize,
    },
    Tokens {
        tab_id: usize,
        input: u32,
        output: u32,
    },
    System {
        tab_id: usize,
        message: String,
    },

    // ── Tab lifecycle ──
    TabOpened {
        tab_id: usize,
        name: String,
        model: String,
    },
    TabClosed {
        tab_id: usize,
    },
    TabRenamed {
        tab_id: usize,
        name: String,
    },
    TabSwitched {
        tab_id: usize,
    },

    // ── Client input ──
    UserMessage {
        tab_id: usize,
        message: String,
    },

    // ── Connection lifecycle ──
    PeerConnected,
    PeerDisconnected,
    Error {
        message: String,
    },
}

// ─── Tab Snapshot (serializable session state) ──────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabSnapshot {
    pub tab_id: usize,
    pub name: String,
    pub model: String,
    pub active: bool,
    pub log: Vec<LogEntrySnapshot>,
    pub tokens: TokenSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSnapshot {
    pub input: u32,
    pub output: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LogEntrySnapshot {
    User { text: String },
    Assistant { text: String },
    System { text: String },
    ToolCall { name: String, detail: String, result: Option<String>, is_error: bool },
}

// ─── Relay Session State ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayStatus {
    Connecting,
    WaitingForClient,
    PairingInProgress,
    Connected { client_count: usize },
    Disconnected,
}

#[derive(Debug, Clone)]
pub struct RelaySession {
    pub hash: String,
    pub status: RelayStatus,
    pub url: String,
    pub created_at: Instant,
}

impl RelaySession {
    pub fn new(hash: String, hub_base_url: &str) -> Self {
        let url = format!("{hub_base_url}/{hash}");
        Self {
            hash,
            status: RelayStatus::Connecting,
            url,
            created_at: Instant::now(),
        }
    }
}

// ─── Client Tracking ────────────────────────────────────────────────────────

#[derive(Debug)]
enum ClientState {
    Pairing(PairingVerifier),
    Paired,
    Rejected,
}

/// Manages relay host state: connected clients and their pairing status.
pub struct RelayHostState {
    clients: HashMap<String, ClientState>,
    /// Channel to notify the TUI when a new pairing code is generated.
    code_display_tx: mpsc::UnboundedSender<(String, String)>, // (client_id, code)
}

impl RelayHostState {
    pub fn new(code_display_tx: mpsc::UnboundedSender<(String, String)>) -> Self {
        Self {
            clients: HashMap::new(),
            code_display_tx,
        }
    }

    /// A new client connected — generate a pairing code and notify the TUI.
    pub fn client_connected(&mut self, client_id: &str) -> String {
        let code = generate_pairing_code();
        let verifier = PairingVerifier::with_defaults(code.clone());
        self.clients.insert(client_id.to_string(), ClientState::Pairing(verifier));
        let _ = self.code_display_tx.send((client_id.to_string(), code.clone()));
        code
    }

    /// Verify a pairing attempt from a client.
    pub fn verify_pairing(&mut self, client_id: &str, code: &str) -> Result<(), PairingError> {
        match self.clients.get_mut(client_id) {
            Some(ClientState::Pairing(verifier)) => {
                let result = verifier.verify(code);
                if result.is_ok() {
                    self.clients.insert(client_id.to_string(), ClientState::Paired);
                } else if matches!(result, Err(PairingError::TooManyAttempts | PairingError::Expired)) {
                    self.clients.insert(client_id.to_string(), ClientState::Rejected);
                }
                result
            }
            Some(ClientState::Paired) => Ok(()), // Already paired
            Some(ClientState::Rejected) => Err(PairingError::TooManyAttempts),
            None => Err(PairingError::Expired), // Unknown client
        }
    }

    /// Check if a client is paired.
    pub fn is_paired(&self, client_id: &str) -> bool {
        matches!(self.clients.get(client_id), Some(ClientState::Paired))
    }

    /// Count of currently paired clients.
    pub fn paired_count(&self) -> usize {
        self.clients.values().filter(|s| matches!(s, ClientState::Paired)).count()
    }

    /// Remove a disconnected client.
    pub fn client_disconnected(&mut self, client_id: &str) {
        self.clients.remove(client_id);
    }
}

// ─── Relay Host (WebSocket client) ──────────────────────────────────────────

/// The relay host manages the WebSocket connection to Passage and bridges
/// events between the CLI and connected web clients.
pub struct RelayHost {
    pub session: RelaySession,
    /// Broadcast channel: CLI events → relay → all paired web clients.
    pub event_tx: broadcast::Sender<RelayMessage>,
    /// Input from web clients → CLI.
    pub input_rx: mpsc::UnboundedReceiver<(usize, String)>, // (tab_id, message)
    /// Internal sender for input (given to the WS read loop).
    input_tx: mpsc::UnboundedSender<(usize, String)>,
    /// State tracking for connected clients.
    state: Arc<Mutex<RelayHostState>>,
}

impl RelayHost {
    /// Create a new relay host session. Does NOT connect yet — call `run()` to start.
    pub fn new(
        hash: String,
        hub_base_url: &str,
        code_display_tx: mpsc::UnboundedSender<(String, String)>,
    ) -> Self {
        let session = RelaySession::new(hash, hub_base_url);
        let (event_tx, _) = broadcast::channel(256);
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let state = Arc::new(Mutex::new(RelayHostState::new(code_display_tx)));

        Self {
            session,
            event_tx,
            input_rx,
            input_tx,
            state,
        }
    }

    /// Get a sender for broadcasting events from the CLI to web clients.
    pub fn event_sender(&self) -> broadcast::Sender<RelayMessage> {
        self.event_tx.clone()
    }

    /// The session hash.
    pub fn hash(&self) -> &str {
        &self.session.hash
    }

    /// The full URL for web access.
    pub fn url(&self) -> &str {
        &self.session.url
    }
}

// ─── Protocol Version ───────────────────────────────────────────────────────

pub const RELAY_PROTOCOL_VERSION: u32 = 1;

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_hash_is_43_chars_base64url() {
        let hash = generate_session_hash();
        assert_eq!(hash.len(), 43);
        assert!(hash.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn pairing_code_is_6_digits() {
        for _ in 0..100 {
            let code = generate_pairing_code();
            assert_eq!(code.len(), 6);
            assert!(code.chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn pairing_verifier_accepts_correct_code() {
        let mut v = PairingVerifier::with_defaults("123456".to_string());
        assert!(v.verify("123456").is_ok());
    }

    #[test]
    fn pairing_verifier_rejects_wrong_code() {
        let mut v = PairingVerifier::with_defaults("123456".to_string());
        let err = v.verify("000000").unwrap_err();
        assert!(matches!(err, PairingError::WrongCode { remaining: 2 }));
    }

    #[test]
    fn pairing_verifier_locks_after_3_attempts() {
        let mut v = PairingVerifier::with_defaults("123456".to_string());
        let _ = v.verify("000000");
        let _ = v.verify("000001");
        let _ = v.verify("000002");
        let err = v.verify("123456").unwrap_err();
        assert!(matches!(err, PairingError::TooManyAttempts));
    }

    #[test]
    fn pairing_verifier_expires() {
        let mut v = PairingVerifier::new("123456".to_string(), Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(10));
        let err = v.verify("123456").unwrap_err();
        assert!(matches!(err, PairingError::Expired));
    }

    #[test]
    fn relay_message_round_trips_json() {
        let msg = RelayMessage::TextDelta {
            tab_id: 0,
            text: "Hello world".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, RelayMessage::TextDelta { tab_id: 0, text } if text == "Hello world"));
    }

    #[test]
    fn relay_message_session_snapshot_serializes() {
        let msg = RelayMessage::SessionSnapshot {
            tabs: vec![TabSnapshot {
                tab_id: 0,
                name: "main".to_string(),
                model: "claude-opus-4-6".to_string(),
                active: true,
                log: vec![
                    LogEntrySnapshot::User { text: "hello".to_string() },
                    LogEntrySnapshot::Assistant { text: "hi there".to_string() },
                ],
                tokens: TokenSnapshot { input: 100, output: 50 },
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("session_snapshot"));
        assert!(json.contains("main"));
        let _: RelayMessage = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn host_state_tracks_clients() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut state = RelayHostState::new(tx);

        let code = state.client_connected("c1");
        assert_eq!(code.len(), 6);
        assert!(!state.is_paired("c1"));
        assert_eq!(state.paired_count(), 0);

        state.verify_pairing("c1", &code).unwrap();
        assert!(state.is_paired("c1"));
        assert_eq!(state.paired_count(), 1);

        state.client_disconnected("c1");
        assert_eq!(state.paired_count(), 0);
    }
}
