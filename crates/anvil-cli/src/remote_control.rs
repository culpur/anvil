// Edition 2024: env::set_var/remove_var require unsafe
#![allow(unsafe_code)]

//! Remote control command handler for `impl LiveCli`.
//!
//! Extracted from `main.rs` to reduce file size.  The `LiveCli` struct and its
//! relay-related fields remain in `main.rs`; only the command implementation
//! lives here.

use crate::LiveCli;

impl LiveCli {
    /// `/remote-control [stop|status]` — manage the web viewer relay session.
    pub(crate) fn run_remote_control_command(&mut self, action: Option<&str>) -> String {
        const HUB_BASE_URL: &str = "https://passage.culpur.net/viewer";

        match action.map_or("", str::trim) {
            "stop" => {
                if self.relay_session.is_none() {
                    return "Remote control: no active session.".to_string();
                }
                self.relay_session = None;
                self.relay_event_tx = None;
                self.relay_input_rx = None;
                "Remote control: session stopped.".to_string()
            }
            "status" => {
                match &self.relay_session {
                    None => "Remote control: no active session.".to_string(),
                    Some(session) => {
                        let client_count = self
                            .relay_event_tx
                            .as_ref()
                            .map_or(0, tokio::sync::broadcast::Sender::receiver_count);
                        format!(
                            "Remote control\n  URL              {}\n  Hash             {}\n  Clients          {}\n  Status           {:?}\n\nNext\n  /remote-control stop   Stop the relay session",
                            session.url,
                            session.hash,
                            client_count,
                            session.status,
                        )
                    }
                }
            }
            // default: start (or report existing session)
            _ => {
                if let Some(session) = &self.relay_session {
                    return format!(
                        "Remote control is already active.\n  URL    {}\n  Hash   {}\n\nUse /remote-control status  to see details.\nUse /remote-control stop    to end the session.",
                        session.url, session.hash
                    );
                }

                let hash = runtime::relay::generate_session_hash();
                let pairing_code = runtime::relay::generate_pairing_code();
                let mut session = runtime::relay::RelaySession::new(hash.clone(), HUB_BASE_URL);
                let url = session.url.clone();
                let (event_tx, _) = tokio::sync::broadcast::channel::<runtime::relay::RelayMessage>(256);

                // Create the relay host with pairing code display channel
                let (code_display_tx, _code_display_rx) = tokio::sync::mpsc::unbounded_channel();
                let relay_host = runtime::relay::RelayHost::new(
                    hash.clone(),
                    HUB_BASE_URL,
                    code_display_tx,
                );

                // Subscribe to the event broadcast for the relay WS loop
                let event_rx = event_tx.subscribe();

                // Spawn the relay WebSocket connection on a background thread
                // using a dedicated tokio runtime (the provider's runtime may not be available)
                let passage_ws_url = "wss://api.culpur.net/v1/relay/sessions".to_string();
                let pairing_code_for_relay = pairing_code.clone();
                // Create a sync channel for receiving user messages from web clients
                let (relay_input_tx, relay_input_rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    else {
                        eprintln!("Failed to create relay tokio runtime");
                        return;
                    };
                    // Set the fixed pairing code BEFORE running the relay
                    rt.block_on(relay_host.set_pairing_code(pairing_code_for_relay));
                    let snapshot_fn = std::sync::Arc::new(tokio::sync::Mutex::new(
                        None::<Box<dyn Fn() -> Vec<runtime::relay::TabSnapshot> + Send>>,
                    ));
                    if let Err(e) = rt.block_on(relay_host.run(&passage_ws_url, event_rx, snapshot_fn, Some(relay_input_tx))) {
                        eprintln!("Relay disconnected: {e}");
                    }
                });

                session.status = runtime::relay::RelayStatus::WaitingForClient;
                session.pairing_code = pairing_code.clone();
                self.relay_session = Some(session);
                self.relay_event_tx = Some(event_tx);
                self.relay_input_rx = Some(relay_input_rx);

                // Auto-open the viewer URL in the default browser
                let _ = open::that(&url);

                format!(
                    "Remote control started\n  URL              {url}\n  Pairing code     {pairing_code}\n  Hash             {hash}\n\nThe URL has been opened in your default browser.\nEnter the pairing code when prompted.\n\nNext\n  /remote-control status   Check connection status\n  /remote-control stop     Stop the relay session"
                )
            }
        }
    }
}
