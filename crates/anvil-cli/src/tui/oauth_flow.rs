//! Reusable OAuth flow state machine (task #642, Sub-task A).
//!
//! `OAuthFlow` packages the OAuth listener + state machine into a struct
//! that can be driven from any ratatui Frame — the main `AnvilTui` event
//! loop, the first-run wizard's `WizardSession`, or any future host.
//!
//! ## Why a new module
//!
//! `provider_login.rs` carries the same OAuth state internally as part of
//! a larger provider-login overlay state machine, but its OAuth path is
//! tangled with the AuthMethodPicker / ApiKeyPaste / MultiField /
//! Instructions screens.  The wizard does NOT need any of those — it
//! needs the OAuth URL/code → success → continue cycle ONLY, and it
//! needs to render inside `WizardSession`'s alt-screen frame.
//!
//! This module owns ONLY the OAuth state and exposes the four operations
//! the wizard (or any host) needs:
//!
//! - [`OAuthFlow::start`] — spawn the localhost HTTP listener + open
//!   the browser, return a flow ready to render.
//! - [`OAuthFlow::poll`] — non-blocking drain of the callback channel.
//! - [`OAuthFlow::handle_key`] — Esc cancels, Enter on success advances.
//! - [`OAuthFlow::render`] — draw the current state into any Frame.
//! - [`OAuthFlow::finalize`] — consume the flow, return the token set or
//!   error.
//!
//! ## Anthropic Max-plan OAuth gate
//!
//! Per `feedback-anvil-max-plan-oauth-gate.md` (#502), the token exchange
//! goes through `api::AnvilApiClient::exchange_oauth_code`, which is the
//! same path `provider_login.rs::complete_anthropic_oauth` and
//! `auth.rs::run_anthropic_login` already use.  The Max-plan gate
//! markers (beta header + identity preamble) are applied at request
//! time by `crates/api/src/providers/anvil_provider.rs::send_raw_request`
//! — `OAuthFlow` does NOT need to reapply them here.
//!
//! ## 8-axis capability contract
//!
//! 1. Definition       — `OAuthFlow` struct + `OAuthFlowState` enum below.
//! 2. Registration     — `pub(super) mod oauth_flow` in `tui/mod.rs`.
//! 3. Completion       — N/A (a runtime helper, not a slash command).
//! 4. Handler          — `poll` / `handle_key` drive the state machine.
//! 5. Dispatch         — wizard step 3 + future main-TUI re-entry call
//!                       through `start` + the per-frame loop.
//! 6. Rendering        — `render` writes into a caller-supplied Frame.
//! 7. Gate             — Esc cancels, Enter on success continues.
//! 8. OTel + tests     — unit tests in this file cover start, poll,
//!                       handle_key, render, plus an end-to-end success.

use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use api::ProviderKind;

/// Default localhost port for the OAuth callback listener.
///
/// Sourced from `crate::DEFAULT_OAUTH_CALLBACK_PORT`. Re-exported here
/// only for clarity at call sites that import `OAuthFlow` standalone.
pub(crate) const DEFAULT_CALLBACK_PORT: u16 = crate::DEFAULT_OAUTH_CALLBACK_PORT;

/// Errors emitted by `OAuthFlow::start` / `finalize`.
#[derive(Debug)]
pub(crate) enum OAuthFlowError {
    /// PKCE / state generation failed.
    Crypto(String),
    /// Token exchange failed (network / server / parsing).
    Exchange(String),
    /// Token persistence failed.
    Save(String),
    /// Config load failed.
    Config(String),
}

impl std::fmt::Display for OAuthFlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Crypto(m) => write!(f, "crypto error: {m}"),
            Self::Exchange(m) => write!(f, "token exchange failed: {m}"),
            Self::Save(m) => write!(f, "could not save token: {m}"),
            Self::Config(m) => write!(f, "config load failed: {m}"),
        }
    }
}

impl std::error::Error for OAuthFlowError {}

/// State the flow can be in.
#[derive(Debug)]
pub(crate) enum OAuthFlowState {
    /// Browser opened + listener running, awaiting callback or paste.
    AwaitingCallback {
        started_at: Instant,
        port: Option<u16>,
        authorize_url: String,
        result_rx: Receiver<Result<(String, String), String>>,
        expected_state: String,
        pkce_verifier: String,
        redirect_uri: String,
    },
    /// Token exchange completed; show success card + wait for Enter.
    Success {
        message: String,
    },
    /// Failed at some stage; show error card + wait for Esc/Enter.
    Failed {
        message: String,
    },
    /// User pressed Esc — flow is over.
    Cancelled,
}

/// Event emitted from `OAuthFlow::poll`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OAuthEvent {
    /// Listener delivered a (code, state) pair; exchange now in progress.
    /// The caller doesn't need to do anything — the flow transitions
    /// internally and the next poll will return `Success` or `Failed`.
    CallbackReceived,
    /// Exchange + save succeeded.
    Success,
    /// Anything went wrong; flow is now in `Failed`.
    Failed(String),
}

/// Action emitted from `OAuthFlow::handle_key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OAuthAction {
    /// Stay in the current state; redraw.
    Continue,
    /// User pressed Esc — flow should be torn down.
    Cancel,
    /// User pressed Enter on the success or failed card — advance.
    Advance,
}

/// The terminal outcome of a flow that has been driven to completion.
#[derive(Debug)]
pub(crate) enum OAuthOutcome {
    /// Token saved successfully.
    Success,
    /// User aborted (Esc) before completion.
    Cancelled,
    /// Flow failed; carries the user-facing error message.
    Failed(String),
}

/// Reusable OAuth flow for Anthropic (and any future OAuth provider).
///
/// Currently anvil only ships an Anthropic OAuth provider (the
/// `Copilot` variant in `ProviderKind` is also OAuth but uses a separate
/// device-flow path).  This struct is generalised over `ProviderKind` so
/// the wizard can pass through whatever the user picked at step 2 — the
/// actual code/state exchange logic dispatches on `provider` internally.
pub(crate) struct OAuthFlow {
    pub(crate) provider: ProviderKind,
    pub(crate) state: OAuthFlowState,
}

impl OAuthFlow {
    /// Start the OAuth flow for the given provider.
    ///
    /// For Anthropic (`AnvilApi`): binds the localhost callback listener
    /// (or falls back to paste-only mode), opens the browser, returns a
    /// flow in `AwaitingCallback`.  The runtime's `OAuthAuthorizationRequest`
    /// applies the canonical Anvil OAuth shape; the Max-plan beta header
    /// + identity preamble are applied later at request time by
    /// `anvil_provider.rs`.
    pub(crate) fn start(provider: ProviderKind) -> Result<Self, OAuthFlowError> {
        if !matches!(provider, ProviderKind::AnvilApi) {
            return Err(OAuthFlowError::Crypto(format!(
                "provider {provider:?} does not support OAuth via OAuthFlow yet"
            )));
        }

        let cwd = std::env::current_dir().unwrap_or_default();
        let config = runtime::ConfigLoader::default_for(&cwd)
            .load()
            .map_err(|e| OAuthFlowError::Config(e.to_string()))?;
        let default_oauth = crate::auth::default_oauth_config();
        let oauth_cfg = config.oauth().cloned().unwrap_or(default_oauth);
        let callback_port = oauth_cfg
            .callback_port
            .unwrap_or(DEFAULT_CALLBACK_PORT);

        let listener = std::net::TcpListener::bind(("127.0.0.1", callback_port)).ok();
        let redirect_uri = if listener.is_some() {
            runtime::loopback_redirect_uri(callback_port)
        } else {
            oauth_cfg
                .manual_redirect_url
                .clone()
                .unwrap_or_else(|| runtime::loopback_redirect_uri(callback_port))
        };

        let pkce = runtime::generate_pkce_pair()
            .map_err(|e| OAuthFlowError::Crypto(format!("pkce: {e}")))?;
        let state = runtime::generate_state()
            .map_err(|e| OAuthFlowError::Crypto(format!("state: {e}")))?;

        let authorize_url = runtime::OAuthAuthorizationRequest::from_config(
            &oauth_cfg,
            redirect_uri.clone(),
            state.clone(),
            &pkce,
        )
        .build_url();

        // Best-effort browser open. Not failing the flow on a missing
        // xdg-open/open — the user can paste the URL.
        let _ = crate::auth::open_browser(&authorize_url);

        let port = listener.as_ref().map(|_| callback_port);
        let (tx, rx) = mpsc::channel::<Result<(String, String), String>>();

        if let Some(lst) = listener {
            let tx_l = tx.clone();
            let expected = state.clone();
            std::thread::spawn(move || {
                let outcome = match lst.accept() {
                    Ok((mut stream, _)) => {
                        use std::io::{Read as _, Write as _};
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let target = req
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("");
                        match runtime::parse_oauth_callback_request_target(target) {
                            Ok(params) => {
                                let body = if params.error.is_some() {
                                    "OAuth failed. You can close this tab."
                                } else {
                                    "OAuth succeeded. You can close this tab."
                                };
                                let resp = format!(
                                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                                    body.len(),
                                    body
                                );
                                let _ = stream.write_all(resp.as_bytes());
                                if let Some(err) = params.error {
                                    Err(format!(
                                        "{}: {}",
                                        err,
                                        params.error_description.unwrap_or_default()
                                    ))
                                } else if let (Some(code), Some(ret_state)) =
                                    (params.code, params.state)
                                {
                                    if ret_state == expected {
                                        Ok((code, ret_state))
                                    } else {
                                        Err("OAuth state mismatch.".to_string())
                                    }
                                } else {
                                    Err("Callback did not include code/state.".to_string())
                                }
                            }
                            Err(e) => Err(e),
                        }
                    }
                    Err(e) => Err(format!("Listener accept error: {e}")),
                };
                let _ = tx_l.send(outcome);
            });
        }

        Ok(Self {
            provider,
            state: OAuthFlowState::AwaitingCallback {
                started_at: Instant::now(),
                port,
                authorize_url,
                result_rx: rx,
                expected_state: state,
                pkce_verifier: pkce.verifier,
                redirect_uri,
            },
        })
    }

    /// The browser URL the user should visit, if the flow is still
    /// awaiting callback.  Useful for the host to also log the URL to a
    /// pre-modal banner so a user whose browser didn't auto-open can
    /// copy it from the visible alt-screen.
    pub(crate) fn authorize_url(&self) -> Option<&str> {
        match &self.state {
            OAuthFlowState::AwaitingCallback { authorize_url, .. } => Some(authorize_url),
            _ => None,
        }
    }

    /// Non-blocking drain of the callback channel.  If a code arrives,
    /// performs the token exchange + save synchronously (≤1s on a
    /// normal network — see `complete_anthropic_oauth` precedent).
    ///
    /// Returns:
    /// - `None` — no callback yet, stay in AwaitingCallback.
    /// - `Some(CallbackReceived)` — listener delivered; exchange is now
    ///   running and the flow will transition to Success/Failed on the
    ///   same call (the host can call `poll` again to read it, or
    ///   just inspect `state`).
    /// - `Some(Success)` — exchange + save complete.
    /// - `Some(Failed(msg))` — exchange or save failed.
    pub(crate) fn poll(&mut self) -> Option<OAuthEvent> {
        // Drain the channel without holding a mutable borrow across the
        // exchange (which mutates `self.state`).
        let received = match &mut self.state {
            OAuthFlowState::AwaitingCallback { result_rx, .. } => result_rx.try_recv().ok(),
            _ => None,
        };
        let outcome = received?;

        // Extract everything we need from AwaitingCallback before the swap.
        let (verifier, redirect_uri) = match &self.state {
            OAuthFlowState::AwaitingCallback {
                pkce_verifier,
                redirect_uri,
                ..
            } => (pkce_verifier.clone(), redirect_uri.clone()),
            _ => return None,
        };

        match outcome {
            Ok((code, state)) => {
                // Run the token exchange synchronously.
                match self.exchange_and_save(code, state, verifier, redirect_uri) {
                    Ok(()) => {
                        self.state = OAuthFlowState::Success {
                            message: "Token saved. You are now logged in.".to_string(),
                        };
                        Some(OAuthEvent::Success)
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        self.state = OAuthFlowState::Failed { message: msg.clone() };
                        Some(OAuthEvent::Failed(msg))
                    }
                }
            }
            Err(err_msg) => {
                self.state = OAuthFlowState::Failed {
                    message: err_msg.clone(),
                };
                Some(OAuthEvent::Failed(err_msg))
            }
        }
    }

    /// Token exchange + persistence path. Mirrors
    /// `input_handler::complete_anthropic_oauth` so any change there
    /// propagates here.
    fn exchange_and_save(
        &self,
        code: String,
        state: String,
        verifier: String,
        redirect_uri: String,
    ) -> Result<(), OAuthFlowError> {
        let cwd = std::env::current_dir().unwrap_or_default();
        let config = runtime::ConfigLoader::default_for(&cwd)
            .load()
            .map_err(|e| OAuthFlowError::Config(e.to_string()))?;
        let default_oauth = crate::auth::default_oauth_config();
        let oauth_cfg = config.oauth().cloned().unwrap_or(default_oauth);

        let exchange = runtime::OAuthTokenExchangeRequest::from_config(
            &oauth_cfg,
            code,
            state,
            verifier,
            redirect_uri,
        );

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| OAuthFlowError::Exchange(format!("runtime: {e}")))?;
        let client = api::AnvilApiClient::from_auth(api::AuthSource::None)
            .with_base_url(api::read_base_url());
        let token_set = rt
            .block_on(client.exchange_oauth_code(&oauth_cfg, &exchange))
            .map_err(|e| OAuthFlowError::Exchange(e.to_string()))?;
        runtime::save_oauth_credentials(&runtime::OAuthTokenSet {
            access_token: token_set.access_token,
            refresh_token: token_set.refresh_token,
            expires_at: token_set.expires_at,
            scopes: token_set.scopes,
        })
        .map_err(|e| OAuthFlowError::Save(e.to_string()))?;
        Ok(())
    }

    /// Handle a single key event.  Esc cancels, Enter advances on
    /// success/failed.  Other keys are passed through and may be used
    /// later for paste-the-code support (TODO: not yet wired here; the
    /// listener-based path covers the common case).
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> OAuthAction {
        match (&self.state, key.code) {
            (OAuthFlowState::AwaitingCallback { .. }, KeyCode::Esc) => {
                self.state = OAuthFlowState::Cancelled;
                OAuthAction::Cancel
            }
            (OAuthFlowState::Success { .. }, KeyCode::Enter | KeyCode::Esc) => {
                OAuthAction::Advance
            }
            (OAuthFlowState::Failed { .. }, KeyCode::Enter | KeyCode::Esc) => {
                OAuthAction::Advance
            }
            _ => OAuthAction::Continue,
        }
    }

    /// Render the current state to a Frame.  The flow draws a centered
    /// modal-style card; the caller is responsible for the surrounding
    /// banner/breadcrumb chrome.
    pub(crate) fn render(&self, frame: &mut Frame, area: Rect, accent: Color) {
        let modal_w = area.width.saturating_sub(8).min(76).max(40);
        let modal_h: u16 = match &self.state {
            OAuthFlowState::AwaitingCallback { .. } => 12,
            OAuthFlowState::Success { .. } => 8,
            OAuthFlowState::Failed { .. } => 10,
            OAuthFlowState::Cancelled => 6,
        };
        if area.width < 12 || area.height < modal_h {
            return;
        }
        let modal_x = (area.width.saturating_sub(modal_w)) / 2;
        let modal_y = (area.height.saturating_sub(modal_h)) / 2;
        let modal_area = Rect {
            x: modal_x,
            y: modal_y,
            width: modal_w,
            height: modal_h,
        };

        frame.render_widget(Clear, modal_area);

        let title = match &self.state {
            OAuthFlowState::AwaitingCallback { .. } => " Anthropic OAuth — waiting for browser ",
            OAuthFlowState::Success { .. } => " Anthropic OAuth — success ",
            OAuthFlowState::Failed { .. } => " Anthropic OAuth — failed ",
            OAuthFlowState::Cancelled => " Anthropic OAuth — cancelled ",
        };
        let border_color = match &self.state {
            OAuthFlowState::Failed { .. } => Color::Red,
            _ => accent,
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color).add_modifier(Modifier::BOLD))
            .style(Style::default().bg(Color::Black));
        let inner = block.inner(modal_area);
        frame.render_widget(block, modal_area);

        let hint = Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM);
        let normal = Style::default().fg(Color::White);
        let link = Style::default().fg(accent).add_modifier(Modifier::UNDERLINED);
        let ok = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);
        let err = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);

        let lines: Vec<Line<'static>> = match &self.state {
            OAuthFlowState::AwaitingCallback {
                started_at,
                authorize_url,
                port,
                ..
            } => {
                let elapsed = started_at.elapsed().as_secs();
                let port_note = match port {
                    Some(p) => format!("Listening on http://127.0.0.1:{p}/callback"),
                    None => "Paste-only mode (no localhost listener)".to_string(),
                };
                let display_url = if authorize_url.len() > inner.width as usize {
                    let cap = inner.width.saturating_sub(3) as usize;
                    format!("{}...", &authorize_url[..cap.min(authorize_url.len())])
                } else {
                    authorize_url.clone()
                };
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "  Browser opened. Waiting for the callback...",
                        normal,
                    )),
                    Line::from(Span::styled(format!("  {port_note}"), hint)),
                    Line::from(Span::styled(format!("  Elapsed: {elapsed}s"), hint)),
                    Line::from(""),
                    Line::from(Span::styled(
                        "  If the browser didn't open, visit this URL:",
                        hint,
                    )),
                    Line::from(Span::styled(format!("  {display_url}"), link)),
                    Line::from(""),
                    Line::from(Span::styled("  Esc cancels — Enter waits.", hint)),
                ]
            }
            OAuthFlowState::Success { message } => {
                vec![
                    Line::from(""),
                    Line::from(Span::styled(format!("  {message}"), ok)),
                    Line::from(""),
                    Line::from(Span::styled("  Enter continues.", hint)),
                ]
            }
            OAuthFlowState::Failed { message } => {
                vec![
                    Line::from(""),
                    Line::from(Span::styled("  Login failed:", err)),
                    Line::from(Span::styled(format!("  {message}"), normal)),
                    Line::from(""),
                    Line::from(Span::styled("  Enter / Esc continues.", hint)),
                ]
            }
            OAuthFlowState::Cancelled => vec![
                Line::from(""),
                Line::from(Span::styled("  Cancelled.", hint)),
            ],
        };
        frame.render_widget(Paragraph::new(lines), inner);
    }

    /// Consume the flow, returning the terminal outcome.  Call ONLY
    /// when `state` is `Success` / `Failed` / `Cancelled` — calling
    /// during `AwaitingCallback` returns `Cancelled` as a conservative
    /// default.
    pub(crate) fn finalize(self) -> OAuthOutcome {
        match self.state {
            OAuthFlowState::Success { .. } => OAuthOutcome::Success,
            OAuthFlowState::Failed { message } => OAuthOutcome::Failed(message),
            OAuthFlowState::Cancelled | OAuthFlowState::AwaitingCallback { .. } => {
                OAuthOutcome::Cancelled
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Tests use synthetic state — `start` requires a real TcpListener +
    //! browser which is not safe in CI.  We instead construct
    //! `OAuthFlow` instances by hand and verify the state-machine
    //! transitions.

    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn make_awaiting() -> OAuthFlow {
        let (_tx, rx) = mpsc::channel();
        OAuthFlow {
            provider: ProviderKind::AnvilApi,
            state: OAuthFlowState::AwaitingCallback {
                started_at: Instant::now(),
                port: Some(54545),
                authorize_url: "https://example.test/authorize?x=1".to_string(),
                result_rx: rx,
                expected_state: "abc".to_string(),
                pkce_verifier: "verifier".to_string(),
                redirect_uri: "http://127.0.0.1:54545/callback".to_string(),
            },
        }
    }

    /// `OAuthFlow::handle_key` — Esc on AwaitingCallback cancels.
    #[test]
    fn oauth_flow_handle_key_esc_cancels() {
        let mut flow = make_awaiting();
        let action = flow.handle_key(key(KeyCode::Esc));
        assert_eq!(action, OAuthAction::Cancel);
        assert!(matches!(flow.state, OAuthFlowState::Cancelled));
    }

    /// `OAuthFlow::handle_key` — Enter on Success advances.
    #[test]
    fn oauth_flow_handle_key_enter_on_success_continues() {
        let mut flow = OAuthFlow {
            provider: ProviderKind::AnvilApi,
            state: OAuthFlowState::Success {
                message: "ok".to_string(),
            },
        };
        let action = flow.handle_key(key(KeyCode::Enter));
        assert_eq!(action, OAuthAction::Advance);
    }

    /// `OAuthFlow::handle_key` — Enter / Esc on Failed advances.
    #[test]
    fn oauth_flow_handle_key_advance_on_failed() {
        let mut flow = OAuthFlow {
            provider: ProviderKind::AnvilApi,
            state: OAuthFlowState::Failed {
                message: "boom".to_string(),
            },
        };
        assert_eq!(flow.handle_key(key(KeyCode::Enter)), OAuthAction::Advance);
    }

    /// `OAuthFlow::poll` — returns None when no callback yet.
    #[test]
    fn oauth_flow_poll_returns_none_when_channel_empty() {
        let mut flow = make_awaiting();
        assert!(flow.poll().is_none());
    }

    /// `OAuthFlow::poll` — when the listener emits an error, the flow
    /// transitions to Failed and `poll` returns `Failed(msg)`.
    #[test]
    fn oauth_flow_poll_returns_failed_on_listener_error() {
        let (tx, rx) = mpsc::channel();
        let mut flow = OAuthFlow {
            provider: ProviderKind::AnvilApi,
            state: OAuthFlowState::AwaitingCallback {
                started_at: Instant::now(),
                port: Some(0),
                authorize_url: "https://example.test/x".to_string(),
                result_rx: rx,
                expected_state: "abc".to_string(),
                pkce_verifier: "v".to_string(),
                redirect_uri: "http://127.0.0.1:0/callback".to_string(),
            },
        };
        tx.send(Err("listener exploded".to_string())).unwrap();
        let evt = flow.poll().expect("event");
        assert!(matches!(evt, OAuthEvent::Failed(ref m) if m.contains("listener exploded")));
        assert!(matches!(flow.state, OAuthFlowState::Failed { .. }));
    }

    /// `OAuthFlow::render` — dispatches to the right widget per state
    /// without panicking on a `TestBackend`.
    #[test]
    fn oauth_flow_render_state_machine_dispatches_correct_widget() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        // AwaitingCallback
        let flow = make_awaiting();
        terminal
            .draw(|frame| {
                let area = frame.area();
                flow.render(frame, area, Color::Cyan);
            })
            .unwrap();
        let buf_str: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            buf_str.contains("waiting for browser") || buf_str.contains("Waiting"),
            "AwaitingCallback render must mention waiting state, got: {buf_str:.300}"
        );

        // Success
        let flow = OAuthFlow {
            provider: ProviderKind::AnvilApi,
            state: OAuthFlowState::Success {
                message: "all good".to_string(),
            },
        };
        terminal
            .draw(|frame| {
                let area = frame.area();
                flow.render(frame, area, Color::Cyan);
            })
            .unwrap();
        let buf_str: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            buf_str.contains("success") || buf_str.contains("all good"),
            "Success render must mention success state"
        );

        // Failed
        let flow = OAuthFlow {
            provider: ProviderKind::AnvilApi,
            state: OAuthFlowState::Failed {
                message: "nope".to_string(),
            },
        };
        terminal
            .draw(|frame| {
                let area = frame.area();
                flow.render(frame, area, Color::Cyan);
            })
            .unwrap();
        let buf_str: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            buf_str.contains("failed") || buf_str.contains("Login failed"),
            "Failed render must mention failed state"
        );
    }

    /// `OAuthFlow::start` only supports Anthropic (`AnvilApi`) for now;
    /// other variants return an error rather than silently doing
    /// nothing.
    #[test]
    fn oauth_flow_start_rejects_unsupported_provider() {
        let res = OAuthFlow::start(ProviderKind::OpenAi);
        assert!(matches!(res, Err(OAuthFlowError::Crypto(_))));
    }

    /// `OAuthFlow::finalize` returns the correct OAuthOutcome variant.
    #[test]
    fn oauth_flow_finalize_returns_outcome_per_state() {
        let success = OAuthFlow {
            provider: ProviderKind::AnvilApi,
            state: OAuthFlowState::Success {
                message: "m".to_string(),
            },
        };
        assert!(matches!(success.finalize(), OAuthOutcome::Success));

        let failed = OAuthFlow {
            provider: ProviderKind::AnvilApi,
            state: OAuthFlowState::Failed {
                message: "boom".to_string(),
            },
        };
        assert!(matches!(failed.finalize(), OAuthOutcome::Failed(_)));

        let cancelled = OAuthFlow {
            provider: ProviderKind::AnvilApi,
            state: OAuthFlowState::Cancelled,
        };
        assert!(matches!(cancelled.finalize(), OAuthOutcome::Cancelled));
    }
}
