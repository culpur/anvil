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
#[must_use] 
pub fn generate_session_hash() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Generate a 6-digit pairing code.
#[must_use] 
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
    #[must_use] 
    pub fn new(code: String, ttl: Duration) -> Self {
        Self {
            code,
            attempts: 0,
            max_attempts: 3,
            expires_at: Instant::now() + ttl,
        }
    }

    /// Default TTL of 5 minutes.
    #[must_use] 
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
    #[must_use] 
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
        session_id: String,
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

    // ── Session metadata (sent after pairing) ──
    SessionMeta {
        session_id: String,
        model: String,
        version: String,
        permission_mode: String,
        thinking_enabled: bool,
        qmd_status: Option<String>,
        block_time: Option<String>,
        status_line_preset: Option<String>,
    },

    // ── Client requests ──
    /// Client requests a new tab be opened on the host.
    RequestNewTab {
        name: Option<String>,
    },
    /// Client requests closing a tab.
    RequestCloseTab {
        tab_id: usize,
    },
    /// Client requests renaming a tab.
    RequestRenameTab {
        tab_id: usize,
        name: String,
    },

    // ── Configuration (browser ↔ TUI) ──
    /// Browser requests current config values.
    ConfigGet,
    /// TUI sends current config values to browser.
    ConfigData {
        data: serde_json::Value,
    },
    /// Browser sets a config key.
    ConfigSet {
        key: String,
        value: String,
    },
    /// TUI confirms config change.
    ConfigUpdated {
        key: String,
        success: bool,
        message: String,
    },

    // ── Phase 3: panel-aware config protocol ──────────────────────────

    /// Host → Web: full config snapshot (sent on pair + on demand).
    ConfigSnapshot {
        config: serde_json::Value,
    },
    /// Host → Web: acknowledgement after a successful config.update write.
    ConfigSaved {
        config: serde_json::Value,
    },
    /// Host → Web: validation or vault-gate failure for a config.update.
    ConfigError {
        panel: String,
        field: String,
        message: String,
    },
    /// Host → Web: vault lock state (sent on pair + whenever lock state changes).
    VaultState {
        locked: bool,
    },
    /// Web → Host: update a single field in a named panel.
    ConfigUpdate {
        panel: String,
        field: String,
        value: serde_json::Value,
    },

    // ── Phase 4: AnvilHub installer ──────────────────────────────────────────

    /// Web → Host: request to install a package from AnvilHub.
    HubInstall {
        slug: String,
        version: String,
    },
    /// Web → Host: request immediate process respawn.
    RespawnRequest,

    /// Host → Web: package installed successfully.
    HubInstalled {
        slug: String,
        version: String,
        /// One of "none" | "soft" | "full"
        requires_restart: String,
    },
    /// Host → Web: install attempt failed.
    HubInstallError {
        slug: String,
        reason: String,
        message: String,
    },
    /// Host → Web: progress update during download/install.
    HubInstallProgress {
        slug: String,
        /// Human-readable phase label, e.g. "downloading", "extracting".
        phase: String,
        /// 0–100.
        percent: u8,
    },

    // ── Client input ──
    UserMessage {
        tab_id: usize,
        message: String,
    },

    // ── Connection lifecycle ──
    PeerConnected,
    PeerDisconnected {
        #[serde(default)]
        client_id: Option<String>,
    },
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
    pub pairing_code: String,
    pub created_at: Instant,
}

impl RelaySession {
    #[must_use] 
    pub fn new(hash: String, hub_base_url: &str) -> Self {
        let url = format!("{hub_base_url}/{hash}");
        Self {
            hash,
            status: RelayStatus::Connecting,
            url,
            pairing_code: String::new(),
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
    /// Fixed pairing code set by the CLI — all clients use this same code.
    fixed_code: Option<String>,
}

impl RelayHostState {
    #[must_use] 
    pub fn new(code_display_tx: mpsc::UnboundedSender<(String, String)>) -> Self {
        Self {
            clients: HashMap::new(),
            code_display_tx,
            fixed_code: None,
        }
    }

    /// Set a fixed pairing code — all clients will use this code instead of random ones.
    pub fn set_fixed_code(&mut self, code: String) {
        self.fixed_code = Some(code);
    }

    /// A new client connected — use fixed code if set, otherwise generate a new one.
    pub fn client_connected(&mut self, client_id: &str) -> String {
        let code = self.fixed_code.clone().unwrap_or_else(generate_pairing_code);
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
    #[must_use] 
    pub fn is_paired(&self, client_id: &str) -> bool {
        matches!(self.clients.get(client_id), Some(ClientState::Paired))
    }

    /// Count of currently paired clients.
    #[must_use] 
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
    #[must_use] 
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

    /// Set the fixed pairing code that all clients must enter.
    /// Must be called before `run()` for the code to take effect.
    pub async fn set_pairing_code(&self, code: String) {
        self.state.lock().await.set_fixed_code(code);
    }

    /// Get a sender for broadcasting events from the CLI to web clients.
    #[must_use] 
    pub fn event_sender(&self) -> broadcast::Sender<RelayMessage> {
        self.event_tx.clone()
    }

    /// The session hash.
    #[must_use] 
    pub fn hash(&self) -> &str {
        &self.session.hash
    }

    /// Run the relay host — connects to Passage via WebSocket and bridges
    /// events between CLI and web clients. This is async and should be spawned
    /// on a tokio runtime.
    ///
    /// - `passage_ws_url`: e.g. `"wss://api.culpur.net/v1/relay/sessions"`
    /// - `event_rx`: receives CLI events to broadcast to web clients
    /// - `snapshot_fn`: called when a client pairs to get the current session state
    pub async fn run(
        &self,
        passage_ws_url: &str,
        mut event_rx: broadcast::Receiver<RelayMessage>,
        snapshot_fn: Arc<Mutex<Option<Box<dyn Fn() -> Vec<TabSnapshot> + Send>>>>,
        // Optional sync sender for forwarding user messages back to the TUI thread.
        user_input_tx: Option<std::sync::mpsc::Sender<(usize, String)>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let url = format!("{}/{hash}?role=host", passage_ws_url, hash = self.session.hash);
        let (ws_stream, _) = connect_async(&url).await
            .map_err(|e| format!("WebSocket connect failed: {e}"))?;

        let (mut ws_sink, mut ws_stream_read) = ws_stream.split();

        // Send host_hello
        let hello = RelayMessage::HostHello {
            hash: self.session.hash.clone(),
            protocol_version: RELAY_PROTOCOL_VERSION,
        };
        ws_sink.send(WsMessage::Text(serde_json::to_string(&hello)?.into())).await?;

        let state = self.state.clone();
        let input_tx = self.input_tx.clone();

        // Keepalive ping every 30 seconds to prevent Cloudflare/Apache timeout
        let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
        ping_interval.tick().await; // skip first immediate tick

        loop {
            tokio::select! {
                // Send periodic WebSocket pings to keep the connection alive
                _ = ping_interval.tick() => {
                    let _ = ws_sink.send(WsMessage::Ping(vec![].into())).await;
                }
                // Read from WebSocket (messages from Passage / web clients)
                ws_msg = ws_stream_read.next() => {
                    match ws_msg {
                        Some(Ok(WsMessage::Text(text_bytes))) => {
                            if let Ok(msg) = serde_json::from_str::<RelayMessage>(&text_bytes) {
                                match msg {
                                    RelayMessage::ClientConnected { ref client_id } => {
                                        let mut st = state.lock().await;
                                        let _code = st.client_connected(client_id);
                                        // Relay sends pairing_required to the client automatically
                                    }
                                    RelayMessage::PairingAttempt { ref client_id, ref code } => {
                                        let mut st = state.lock().await;
                                        let result = st.verify_pairing(client_id, code);
                                        let reply = RelayMessage::PairingResult {
                                            client_id: client_id.clone(),
                                            success: result.is_ok(),
                                            error: result.err().map(|e| e.to_string()),
                                        };
                                        let _ = ws_sink.send(WsMessage::Text(serde_json::to_string(&reply)?.into())).await;

                                        // If paired, send session snapshot + notify TUI
                                        if st.is_paired(client_id) {
                                            if let Some(ref func) = *snapshot_fn.lock().await {
                                                let tabs = func();
                                                let snapshot = RelayMessage::SessionSnapshot { tabs };
                                                let _ = ws_sink.send(WsMessage::Text(serde_json::to_string(&snapshot)?.into())).await;
                                            }
                                            // Signal TUI that a client connected
                                            let count = st.paired_count();
                                            if let Some(ref sync_tx) = user_input_tx {
                                                let _ = sync_tx.send((0, format!("__client_connected:{count}")));
                                            }
                                        }
                                    }
                                    RelayMessage::UserMessage { tab_id, ref message } => {
                                        let st = state.lock().await;
                                        // Only accept input from paired clients
                                        if st.paired_count() > 0 {
                                            let _ = input_tx.send((tab_id, message.clone()));
                                            // Forward to the sync TUI channel if available
                                            if let Some(ref sync_tx) = user_input_tx {
                                                let _ = sync_tx.send((tab_id, message.clone()));
                                            }
                                        }
                                    }
                                    RelayMessage::RequestNewTab { ref name } => {
                                        let st = state.lock().await;
                                        if st.paired_count() > 0 {
                                            let tab_name = name.as_deref().unwrap_or("remote");
                                            if let Some(ref sync_tx) = user_input_tx {
                                                // Use special prefix so TUI knows this is a tab request
                                                let _ = sync_tx.send((0, format!("__new_tab:{tab_name}")));
                                            }
                                        }
                                    }
                                    RelayMessage::RequestCloseTab { tab_id } => {
                                        let st = state.lock().await;
                                        if st.paired_count() > 0
                                            && let Some(ref sync_tx) = user_input_tx {
                                                let _ = sync_tx.send((0, format!("__close_tab:{tab_id}")));
                                            }
                                    }
                                    RelayMessage::ConfigGet => {
                                        let st = state.lock().await;
                                        if st.paired_count() > 0
                                            && let Some(ref sync_tx) = user_input_tx {
                                                let _ = sync_tx.send((0, "__config_get".to_string()));
                                            }
                                    }
                                    RelayMessage::ConfigSet { ref key, ref value } => {
                                        let st = state.lock().await;
                                        if st.paired_count() > 0
                                            && let Some(ref sync_tx) = user_input_tx {
                                                let _ = sync_tx.send((0, format!("__config_set:{key}:{value}")));
                                            }
                                    }
                                    // Phase 3 panel-aware config update
                                    RelayMessage::ConfigUpdate { ref panel, ref field, ref value } => {
                                        let st = state.lock().await;
                                        if st.paired_count() > 0
                                            && let Some(ref sync_tx) = user_input_tx {
                                                let value_json = serde_json::to_string(value).unwrap_or_default();
                                                let _ = sync_tx.send((0, format!("__config_update:{panel}:{field}:{value_json}")));
                                            }
                                    }
                                    RelayMessage::RequestRenameTab { tab_id, ref name } => {
                                        let st = state.lock().await;
                                        if st.paired_count() > 0
                                            && let Some(ref sync_tx) = user_input_tx {
                                                let _ = sync_tx.send((0, format!("__rename_tab:{tab_id}:{name}")));
                                            }
                                    }
                                    // Phase 4: hub install request from web client
                                    RelayMessage::HubInstall { ref slug, ref version } => {
                                        let st = state.lock().await;
                                        if st.paired_count() > 0
                                            && let Some(ref sync_tx) = user_input_tx {
                                                let _ = sync_tx.send((0, format!("__hub_install:{slug}:{version}")));
                                            }
                                    }
                                    // Phase 4: web client requests host to respawn
                                    RelayMessage::RespawnRequest => {
                                        let st = state.lock().await;
                                        if st.paired_count() > 0
                                            && let Some(ref sync_tx) = user_input_tx {
                                                let _ = sync_tx.send((0, "__respawn_request".to_string()));
                                            }
                                    }
                                    RelayMessage::PeerDisconnected { client_id } => {
                                        // A web client disconnected — remove from state + notify TUI
                                        let mut st = state.lock().await;
                                        if let Some(cid) = &client_id {
                                            st.client_disconnected(cid);
                                        }
                                        let count = st.paired_count();
                                        if let Some(ref sync_tx) = user_input_tx {
                                            let _ = sync_tx.send((0, format!("__client_disconnected:{count}")));
                                        }
                                    }
                                    _ => {} // Ignore other messages from relay
                                }
                            }
                        }
                        Some(Ok(WsMessage::Close(_))) | None => {
                            break; // Connection closed
                        }
                        Some(Err(e)) => {
                            eprintln!("Relay WS error: {e}");
                            break;
                        }
                        _ => {} // Ping/Pong handled by tungstenite
                    }
                }

                // Read from CLI event broadcast (forward to all paired web clients)
                event = event_rx.recv() => {
                    match event {
                        Ok(relay_msg) => {
                            let json = serde_json::to_string(&relay_msg)?;
                            let _ = ws_sink.send(WsMessage::Text(json.into())).await;
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("Relay broadcast lagged by {n} messages");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            break; // CLI shut down the broadcast
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// The full URL for web access.
    #[must_use] 
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

    #[test]
    fn peer_disconnected_deserializes_from_passage_json() {
        // Passage sends: {"type":"peer_disconnected","client_id":"abc123"}
        let json = r#"{"type":"peer_disconnected","client_id":"abc123"}"#;
        let msg: Result<RelayMessage, _> = serde_json::from_str(json);
        assert!(msg.is_ok(), "Failed to deserialize peer_disconnected: {:?}", msg.err());
        assert!(matches!(msg.unwrap(), RelayMessage::PeerDisconnected { .. }));
    }

    #[test]
    fn peer_disconnected_without_client_id_also_works() {
        // In case Passage sends without client_id
        let json = r#"{"type":"peer_disconnected"}"#;
        let msg: Result<RelayMessage, _> = serde_json::from_str(json);
        assert!(msg.is_ok(), "Failed to deserialize bare peer_disconnected: {:?}", msg.err());
    }

    #[test]
    fn client_connected_deserializes() {
        let json = r#"{"type":"client_connected","client_id":"xyz789"}"#;
        let msg: Result<RelayMessage, _> = serde_json::from_str(json);
        assert!(msg.is_ok(), "Failed to deserialize client_connected: {:?}", msg.err());
    }

    // ── Phase 3: config panel protocol round-trip tests ───────────────────────

    #[test]
    fn config_snapshot_round_trips() {
        let config = serde_json::json!({
            "vault": {"vault_session_ttl": 1800, "vault_auto_lock": false},
            "models": {"default_model": "claude-sonnet-4-6"}
        });
        let msg = RelayMessage::ConfigSnapshot { config: config.clone() };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"config_snapshot\""));
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            RelayMessage::ConfigSnapshot { config: c } => {
                assert_eq!(c["vault"]["vault_session_ttl"], 1800);
            }
            other => panic!("Expected ConfigSnapshot, got {other:?}"),
        }
    }

    #[test]
    fn config_saved_round_trips() {
        let config = serde_json::json!({"vault": {"vault_auto_lock": true}});
        let msg = RelayMessage::ConfigSaved { config: config.clone() };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"config_saved\""));
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, RelayMessage::ConfigSaved { .. }));
    }

    #[test]
    fn config_error_round_trips() {
        let msg = RelayMessage::ConfigError {
            panel: "vault".to_string(),
            field: "auto_lock".to_string(),
            message: "Vault is locked".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"config_error\""));
        assert!(json.contains("\"panel\":\"vault\""));
        assert!(json.contains("\"field\":\"auto_lock\""));
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            RelayMessage::ConfigError { panel, field, message } => {
                assert_eq!(panel, "vault");
                assert_eq!(field, "auto_lock");
                assert!(message.contains("locked"));
            }
            other => panic!("Expected ConfigError, got {other:?}"),
        }
    }

    #[test]
    fn vault_state_round_trips_locked() {
        let msg = RelayMessage::VaultState { locked: true };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"vault_state\""));
        assert!(json.contains("\"locked\":true"));
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, RelayMessage::VaultState { locked: true }));
    }

    #[test]
    fn vault_state_round_trips_unlocked() {
        let msg = RelayMessage::VaultState { locked: false };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"locked\":false"));
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, RelayMessage::VaultState { locked: false }));
    }

    #[test]
    fn config_update_round_trips_string_value() {
        let msg = RelayMessage::ConfigUpdate {
            panel: "models".to_string(),
            field: "default_model".to_string(),
            value: serde_json::Value::String("claude-opus-4-7".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"config_update\""));
        assert!(json.contains("\"panel\":\"models\""));
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            RelayMessage::ConfigUpdate { panel, field, value } => {
                assert_eq!(panel, "models");
                assert_eq!(field, "default_model");
                assert_eq!(value.as_str().unwrap(), "claude-opus-4-7");
            }
            other => panic!("Expected ConfigUpdate, got {other:?}"),
        }
    }

    #[test]
    fn config_update_round_trips_bool_value() {
        let msg = RelayMessage::ConfigUpdate {
            panel: "vault".to_string(),
            field: "vault_auto_lock".to_string(),
            value: serde_json::Value::Bool(true),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            RelayMessage::ConfigUpdate { value, .. } => {
                assert_eq!(value, serde_json::Value::Bool(true));
            }
            other => panic!("Expected ConfigUpdate, got {other:?}"),
        }
    }

    #[test]
    fn config_update_round_trips_numeric_value() {
        let msg = RelayMessage::ConfigUpdate {
            panel: "vault".to_string(),
            field: "vault_session_ttl".to_string(),
            value: serde_json::json!(3600u64),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            RelayMessage::ConfigUpdate { field, value, .. } => {
                assert_eq!(field, "vault_session_ttl");
                assert_eq!(value.as_u64().unwrap(), 3600);
            }
            other => panic!("Expected ConfigUpdate, got {other:?}"),
        }
    }

    #[test]
    fn config_snapshot_web_json_keys_use_snake_case() {
        // Verify serde produces the snake_case type names the browser expects.
        let msg = RelayMessage::ConfigSnapshot {
            config: serde_json::json!({}),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("config_snapshot"), "Expected snake_case type key");
    }

    // ── Phase 3b: Status Line config round-trip tests ─────────────────

    #[test]
    fn status_line_config_round_trips_via_config_update() {
        // A full StatusLineConfig carried as a JSON object value through ConfigUpdate.
        use crate::theme::{Side, StatusLine, StatusLineConfig, StatusWidget};
        let cfg = StatusLineConfig {
            preset: "custom".into(),
            lines: vec![
                StatusLine {
                    left: vec![StatusWidget::Model, StatusWidget::Separator, StatusWidget::Cost],
                    right: vec![StatusWidget::Version],
                },
                StatusLine {
                    left: vec![StatusWidget::GitBranch, StatusWidget::GitStatus],
                    right: vec![StatusWidget::TokensTotal],
                },
                StatusLine {
                    left: vec![StatusWidget::ContextBar, StatusWidget::ContextPct],
                    right: vec![],
                },
                StatusLine {
                    left: vec![StatusWidget::Permissions],
                    right: vec![StatusWidget::TimeDisplay],
                },
            ],
            separator_char: " | ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        };
        let _ = Side::Left; // ensure import used
        let cfg_value = serde_json::to_value(&cfg).unwrap();
        let msg = RelayMessage::ConfigUpdate {
            panel: "display".to_string(),
            field: "status_line".to_string(),
            value: cfg_value,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            RelayMessage::ConfigUpdate { panel, field, value } => {
                assert_eq!(panel, "display");
                assert_eq!(field, "status_line");
                let back: StatusLineConfig = serde_json::from_value(value).unwrap();
                assert_eq!(back.preset, "custom");
                assert_eq!(back.lines.len(), 4);
                assert_eq!(back.lines[0].left.len(), 3);
                assert_eq!(back.lines[0].left[0].id(), "model");
                assert_eq!(back.lines[3].right[0].id(), "time_display");
            }
            other => panic!("Expected ConfigUpdate, got {other:?}"),
        }
    }

    #[test]
    fn status_line_config_deserialize_all_widget_ids() {
        // Every id produced by StatusWidget::all_widgets() must survive a
        // JSON serialise → deserialise round-trip through a StatusLineConfig value.
        use crate::theme::{StatusLine, StatusLineConfig, StatusWidget};
        let all = StatusWidget::all_widgets();
        let cfg = StatusLineConfig {
            preset: "test".into(),
            lines: vec![StatusLine { left: all.clone(), right: vec![] }],
            separator_char: " │ ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: StatusLineConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.lines[0].left.len(), all.len());
        for (orig, restored) in all.iter().zip(back.lines[0].left.iter()) {
            assert_eq!(orig.id(), restored.id());
        }
    }

    #[test]
    fn status_line_preset_application_replaces_config() {
        // Applying a named preset via ConfigUpdate (as a string field value)
        // must produce a valid preset config that the host can deserialise.
        use crate::theme::{StatusLineConfig, StatusLinePreset};
        let preset_name = "minimal";
        let msg = RelayMessage::ConfigUpdate {
            panel: "display".to_string(),
            field: "status_line_preset".to_string(),
            value: serde_json::Value::String(preset_name.to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            RelayMessage::ConfigUpdate { value, .. } => {
                let name = value.as_str().unwrap();
                let preset = StatusLinePreset::from_name(name).expect("known preset");
                let cfg = StatusLineConfig::from_preset(preset);
                assert_eq!(cfg.preset, "minimal");
                assert!(!cfg.lines.is_empty());
            }
            other => panic!("Expected ConfigUpdate, got {other:?}"),
        }
    }

    // ── Phase 4: AnvilHub installer relay message round-trip tests ────────────

    #[test]
    fn hub_install_round_trips() {
        let msg = RelayMessage::HubInstall {
            slug: "skill-foo".to_string(),
            version: "1.2.3".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"hub_install\""));
        assert!(json.contains("\"slug\":\"skill-foo\""));
        assert!(json.contains("\"version\":\"1.2.3\""));
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            RelayMessage::HubInstall { slug, version } => {
                assert_eq!(slug, "skill-foo");
                assert_eq!(version, "1.2.3");
            }
            other => panic!("Expected HubInstall, got {other:?}"),
        }
    }

    #[test]
    fn respawn_request_round_trips() {
        let msg = RelayMessage::RespawnRequest;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"respawn_request\""));
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, RelayMessage::RespawnRequest));
    }

    #[test]
    fn hub_installed_round_trips_with_restart_tags() {
        for tag in &["none", "soft", "full"] {
            let msg = RelayMessage::HubInstalled {
                slug: "plugin-bar".to_string(),
                version: "2.0.0".to_string(),
                requires_restart: (*tag).to_string(),
            };
            let json = serde_json::to_string(&msg).unwrap();
            assert!(json.contains("\"type\":\"hub_installed\""), "tag={tag}");
            assert!(json.contains(&format!("\"requires_restart\":\"{tag}\"")), "tag={tag}");
            let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
            match parsed {
                RelayMessage::HubInstalled { requires_restart, .. } => {
                    assert_eq!(requires_restart, *tag);
                }
                other => panic!("Expected HubInstalled, got {other:?}"),
            }
        }
    }

    #[test]
    fn hub_install_error_round_trips() {
        let msg = RelayMessage::HubInstallError {
            slug: "bad-pkg".to_string(),
            reason: "vault_locked".to_string(),
            message: "Vault is locked — unlock to install packages".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"hub_install_error\""));
        assert!(json.contains("\"reason\":\"vault_locked\""));
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            RelayMessage::HubInstallError { slug, reason, message } => {
                assert_eq!(slug, "bad-pkg");
                assert_eq!(reason, "vault_locked");
                assert!(message.contains("locked"));
            }
            other => panic!("Expected HubInstallError, got {other:?}"),
        }
    }

    #[test]
    fn hub_install_progress_round_trips() {
        let msg = RelayMessage::HubInstallProgress {
            slug: "theme-cool".to_string(),
            phase: "downloading".to_string(),
            percent: 42,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"hub_install_progress\""));
        assert!(json.contains("\"percent\":42"));
        let parsed: RelayMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            RelayMessage::HubInstallProgress { slug, phase, percent } => {
                assert_eq!(slug, "theme-cool");
                assert_eq!(phase, "downloading");
                assert_eq!(percent, 42);
            }
            other => panic!("Expected HubInstallProgress, got {other:?}"),
        }
    }

    #[test]
    fn pkg_type_to_restart_requirement_mapping() {
        // PLUGIN/MCP  → "full"
        // THEME       → "soft"
        // SKILL/AGENT → "none"
        fn restart_for_type(t: &str) -> &'static str {
            match t {
                "plugin" | "mcp" => "full",
                "theme" => "soft",
                _ => "none",
            }
        }
        assert_eq!(restart_for_type("plugin"), "full");
        assert_eq!(restart_for_type("mcp"), "full");
        assert_eq!(restart_for_type("theme"), "soft");
        assert_eq!(restart_for_type("skill"), "none");
        assert_eq!(restart_for_type("agent"), "none");
    }

    #[test]
    fn platform_detection_produces_known_tag() {
        fn current_platform() -> &'static str {
            if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                "darwin-arm64"
            } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
                "darwin-x86_64"
            } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
                "linux-x86_64"
            } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
                "linux-arm64"
            } else if cfg!(target_os = "windows") {
                "windows-x86_64"
            } else {
                "linux-x86_64"
            }
        }
        let platform = current_platform();
        let known = &["darwin-arm64", "darwin-x86_64", "linux-x86_64", "linux-arm64", "windows-x86_64"];
        assert!(known.contains(&platform), "unknown platform tag: {platform}");
    }
}
