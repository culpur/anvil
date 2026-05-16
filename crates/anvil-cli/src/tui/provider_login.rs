//! In-TUI provider login modal (#578).
//!
//! Replaces the drop-to-CLI pattern in `/provider <name> login` with a
//! centered overlay that never leaves the alternate screen.
//!
//! ## Screen state machine
//!
//! ```text
//!                        open(provider)
//!                             │
//!                    ┌────────▼────────┐
//!                    │ AuthMethodPicker │  ← only for providers with choice
//!                    └──┬──────────┬───┘
//!                       │          │
//!            (API key)  │          │  (OAuth/device)
//!                       ▼          ▼
//!                  ApiKeyPaste  OAuthWaiting  ← also: MultiField (Ollama/Azure)
//!                       │          │
//!                       └────┬─────┘
//!                            │
//!                         ┌──▼──┐
//!                         │Result│  ← ok:true or ok:false
//!                         └──┬──┘
//!                            │  Esc / Enter
//!                          (closed)
//! ```
//!
//! ## Non-terminal mode fallback
//!
//! When `std::io::IsTerminal::is_terminal` is false (headless CI,
//! `anvil --print`) the modal is never opened; callers detect the
//! `tui_active` flag and fall back to the existing CLI path.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use api::ProviderKind;

// ─── FormField ───────────────────────────────────────────────────────────────

/// A single named text-input field inside a `MultiField` screen.
#[derive(Debug, Clone)]
pub struct FormField {
    pub label: &'static str,
    pub value: String,
    pub cursor: usize,
    pub masked: bool,
    pub placeholder: &'static str,
}

impl FormField {
    pub fn new(label: &'static str, placeholder: &'static str) -> Self {
        Self {
            label,
            value: String::new(),
            cursor: 0,
            masked: false,
            placeholder,
        }
    }
    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }
    pub fn display(&self) -> String {
        if self.masked {
            "*".repeat(self.value.len())
        } else {
            self.value.clone()
        }
    }
}

// ─── ProviderLoginScreen ─────────────────────────────────────────────────────

/// Which screen the provider-login overlay is showing.
// Not Clone because OAuthWaiting contains an mpsc::Receiver.
#[derive(Debug)]
pub enum ProviderLoginScreen {
    /// Choose OAuth vs API key (for providers that offer both).
    AuthMethodPicker {
        provider: ProviderKind,
        /// Display labels for each method.
        methods: Vec<&'static str>,
        selected: usize,
    },
    /// API-key paste screen (single masked input + prefix hint).
    ApiKeyPaste {
        provider: ProviderKind,
        input: String,
        cursor: usize,
        env_var: &'static str,
        vault_key: &'static str,
        prefix: &'static str,
        error: Option<String>,
    },
    /// OAuth browser-flow screen: browser opened, waiting for callback or paste.
    OAuthWaiting {
        provider: ProviderKind,
        started_at: Instant,
        /// The localhost port the listener is bound on (None = paste-only mode).
        port: Option<u16>,
        /// The authorization URL shown to the user if the browser didn't open.
        authorize_url: String,
        /// User can paste the callback URL or bare auth code here instead.
        fallback_url_input: String,
        fallback_cursor: usize,
        /// Channel for the background listener thread to send the code+state.
        result_rx: Option<std::sync::mpsc::Receiver<Result<(String, String), String>>>,
        /// Expected OAuth state (for validation).
        expected_state: String,
        /// PKCE verifier (stored for token exchange).
        pkce_verifier: String,
        /// Redirect URI used when starting the flow.
        redirect_uri: String,
    },
    /// Multi-field form (used for Ollama, Azure, AWS Bedrock).
    MultiField {
        provider: ProviderKind,
        fields: Vec<FormField>,
        focused: usize,
        error: Option<String>,
    },
    /// Instructions-only screen (Azure / AWS Bedrock).
    Instructions {
        provider: ProviderKind,
        lines: Vec<&'static str>,
    },
    /// Terminal result screen.
    Result {
        provider: ProviderKind,
        ok: bool,
        message: String,
    },
}

// ─── ProviderLoginRenderSnapshot ─────────────────────────────────────────────

/// A Clone-able, draw-ready snapshot of the provider-login modal state.
///
/// `ProviderLoginModal` contains a `mpsc::Receiver` (not `Clone`), so it
/// cannot be copied into the `LayoutSnapshot`.  This struct strips that field
/// and carries only the data needed for rendering.
#[derive(Clone)]
pub struct ProviderLoginRenderSnapshot {
    pub screen: ProviderLoginRenderScreen,
}

/// The render-only mirror of `ProviderLoginScreen` (no `Receiver`).
#[derive(Clone)]
pub enum ProviderLoginRenderScreen {
    AuthMethodPicker {
        provider: ProviderKind,
        methods: Vec<&'static str>,
        selected: usize,
    },
    ApiKeyPaste {
        provider: ProviderKind,
        input: String,
        env_var: &'static str,
        prefix: &'static str,
        error: Option<String>,
    },
    OAuthWaiting {
        provider: ProviderKind,
        started_at: Instant,
        authorize_url: String,
        fallback_url_input: String,
    },
    MultiField {
        provider: ProviderKind,
        fields: Vec<FormField>,
        focused: usize,
        error: Option<String>,
    },
    Instructions {
        provider: ProviderKind,
        lines: Vec<&'static str>,
    },
    Result {
        provider: ProviderKind,
        ok: bool,
        message: String,
    },
}

impl ProviderLoginRenderSnapshot {
    /// Render the snapshot as a modal overlay.
    pub fn render(&self, frame: &mut Frame, area: Rect, theme_accent: Color, theme_error: Color) {
        // Delegate to a helper that mirrors the main render() logic.
        render_snapshot_impl(self, frame, area, theme_accent, theme_error);
    }
}

// ─── ProviderLoginModal ──────────────────────────────────────────────────────

/// The provider-login overlay state machine.
pub struct ProviderLoginModal {
    pub screen: ProviderLoginScreen,
}

impl ProviderLoginModal {
    /// Open a modal for the given provider, choosing the right initial screen.
    pub fn open(provider: ProviderKind) -> Self {
        let screen = match provider {
            ProviderKind::AnvilApi => ProviderLoginScreen::AuthMethodPicker {
                provider,
                methods: vec!["OAuth (browser via claude.ai)", "API Key"],
                selected: 0,
            },
            ProviderKind::OpenAi => ProviderLoginScreen::ApiKeyPaste {
                provider,
                input: String::new(),
                cursor: 0,
                env_var: "OPENAI_API_KEY",
                vault_key: "openai_api_key",
                prefix: "sk-",
                error: None,
            },
            ProviderKind::Gemini => ProviderLoginScreen::ApiKeyPaste {
                provider,
                input: String::new(),
                cursor: 0,
                env_var: "GEMINI_API_KEY",
                vault_key: "gemini_api_key",
                prefix: "AIza",
                error: None,
            },
            ProviderKind::Xai => ProviderLoginScreen::ApiKeyPaste {
                provider,
                input: String::new(),
                cursor: 0,
                env_var: "XAI_API_KEY",
                vault_key: "xai_api_key",
                prefix: "xai-",
                error: None,
            },
            ProviderKind::Ollama => ProviderLoginScreen::MultiField {
                provider,
                fields: vec![
                    FormField::new("Endpoint", "http://localhost:11434"),
                    FormField::new("API Key (optional)", "").masked(),
                ],
                focused: 0,
                error: None,
            },
            ProviderKind::Azure => ProviderLoginScreen::Instructions {
                provider,
                lines: vec![
                    "Set these environment variables:",
                    "",
                    "  AZURE_OPENAI_ENDPOINT         e.g. https://MY.openai.azure.com",
                    "  AZURE_OPENAI_DEPLOYMENT_NAME  e.g. gpt-4o",
                    "  AZURE_OPENAI_API_VERSION      e.g. 2025-01-01-preview",
                    "  AZURE_OPENAI_API_KEY          your api-key",
                    "  AZURE_AD_TOKEN                AAD bearer token (optional)",
                    "",
                    "Press Esc to close.",
                ],
            },
            ProviderKind::Bedrock => ProviderLoginScreen::Instructions {
                provider,
                lines: vec![
                    "Set these environment variables:",
                    "",
                    "  AWS_ACCESS_KEY_ID       your access key",
                    "  AWS_SECRET_ACCESS_KEY   your secret key",
                    "  AWS_REGION              e.g. us-east-1",
                    "  AWS_SESSION_TOKEN       (optional, temporary creds)",
                    "",
                    "Or run: aws configure",
                    "",
                    "Press Esc to close.",
                ],
            },
            ProviderKind::Copilot => ProviderLoginScreen::OAuthWaiting {
                provider,
                started_at: Instant::now(),
                port: None,
                authorize_url: String::new(),
                fallback_url_input: String::new(),
                fallback_cursor: 0,
                result_rx: None,
                expected_state: String::new(),
                pkce_verifier: String::new(),
                redirect_uri: String::new(),
            },
            // Group B: OpenAI-compatible API-key providers — delegate to the
            // dynamic slug-based lookup via api_key_screen_for_kind().
            _ => api_key_screen_for_kind(provider),
        };
        Self { screen }
    }

    /// Title to show in the modal border.
    pub fn title(&self) -> String {
        match &self.screen {
            ProviderLoginScreen::AuthMethodPicker { provider, .. } => {
                format!(" {} Login ", provider_display(provider))
            }
            ProviderLoginScreen::ApiKeyPaste { provider, .. } => {
                format!(" {} API Key ", provider_display(provider))
            }
            ProviderLoginScreen::OAuthWaiting { provider, .. } => {
                format!(" {} OAuth ", provider_display(provider))
            }
            ProviderLoginScreen::MultiField { provider, .. } => {
                format!(" {} Setup ", provider_display(provider))
            }
            ProviderLoginScreen::Instructions { provider, .. } => {
                format!(" {} Setup ", provider_display(provider))
            }
            ProviderLoginScreen::Result { ok, provider, .. } => {
                if *ok {
                    format!(" {} Login Complete ", provider_display(provider))
                } else {
                    format!(" {} Login Failed ", provider_display(provider))
                }
            }
        }
    }

    /// Render the modal overlay onto the frame.
    pub fn render(&self, frame: &mut Frame, area: Rect, theme_accent: Color, theme_error: Color) {
        let modal_w = (area.width.saturating_sub(8)).min(70);
        let modal_h = self.preferred_height().min(area.height.saturating_sub(4));
        let modal_x = (area.width.saturating_sub(modal_w)) / 2;
        let modal_y = (area.height.saturating_sub(modal_h)) / 2;
        let modal_area = Rect {
            x: modal_x,
            y: modal_y,
            width: modal_w,
            height: modal_h,
        };

        frame.render_widget(Clear, modal_area);

        let border_color = match &self.screen {
            ProviderLoginScreen::Result { ok: false, .. } => theme_error,
            _ => theme_accent,
        };

        let title = self.title();
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color).add_modifier(Modifier::BOLD))
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(modal_area);
        frame.render_widget(block, modal_area);

        let lines = self.build_lines(inner.width as usize, theme_accent, theme_error);
        let para = Paragraph::new(Text::from(lines));
        frame.render_widget(para, inner);
    }

    fn preferred_height(&self) -> u16 {
        match &self.screen {
            ProviderLoginScreen::AuthMethodPicker { methods, .. } => {
                (6 + methods.len()) as u16
            }
            ProviderLoginScreen::ApiKeyPaste { .. } => 12,
            ProviderLoginScreen::OAuthWaiting { .. } => 14,
            ProviderLoginScreen::MultiField { fields, .. } => (8 + fields.len() * 3) as u16,
            ProviderLoginScreen::Instructions { lines, .. } => (4 + lines.len()) as u16,
            ProviderLoginScreen::Result { .. } => 8,
        }
    }

    fn build_lines(&self, width: usize, accent: Color, error_color: Color) -> Vec<Line<'static>> {
        let label_style = Style::default().fg(Color::DarkGray);
        let selected_style = Style::default()
            .fg(Color::Black)
            .bg(accent)
            .add_modifier(Modifier::BOLD);
        let normal_style = Style::default().fg(Color::White);
        let error_style = Style::default().fg(error_color);
        let hint_style = Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM);
        let link_style = Style::default().fg(accent).add_modifier(Modifier::UNDERLINED);

        let _ = (width, label_style, selected_style, normal_style, error_style, hint_style, link_style);

        match &self.screen {
            ProviderLoginScreen::AuthMethodPicker { methods, selected, .. } => {
                let mut out = vec![Line::from(""), Line::from(Span::styled("  Select login method:", label_style))];
                for (i, method) in methods.iter().enumerate() {
                    let (prefix, style) = if i == *selected {
                        ("  > ", selected_style)
                    } else {
                        ("    ", normal_style)
                    };
                    out.push(Line::from(Span::styled(
                        format!("{prefix}{method}"),
                        style,
                    )));
                }
                out.push(Line::from(""));
                out.push(Line::from(Span::styled("  Up/Down: select   Enter: confirm   Esc: cancel", hint_style)));
                out
            }

            ProviderLoginScreen::ApiKeyPaste { env_var, prefix, input, error, .. } => {
                let display: String = if input.is_empty() {
                    String::new()
                } else {
                    let visible = 4.min(input.len());
                    let hidden = input.len().saturating_sub(visible);
                    format!("{}{}", &input[..visible], "*".repeat(hidden))
                };
                let mut out = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("  Env var: {env_var}"),
                        label_style,
                    )),
                    Line::from(Span::styled(
                        format!("  Prefix:  {}", if prefix.is_empty() { "(none)" } else { prefix }),
                        hint_style,
                    )),
                    Line::from(""),
                    Line::from(Span::styled("  API Key:", label_style)),
                    Line::from(Span::styled(
                        format!("  {}", if display.is_empty() { "<paste key here>" } else { &display }),
                        if display.is_empty() { hint_style } else { Style::default().fg(Color::Cyan) },
                    )),
                    Line::from(""),
                ];
                if let Some(err) = error {
                    out.push(Line::from(Span::styled(format!("  Error: {err}"), error_style)));
                    out.push(Line::from(""));
                }
                out.push(Line::from(Span::styled("  Paste key + Enter to save   Esc: cancel", hint_style)));
                out
            }

            ProviderLoginScreen::OAuthWaiting {
                started_at,
                authorize_url,
                fallback_url_input,
                result_rx: _,
                ..
            } => {
                let elapsed = started_at.elapsed().as_secs();
                let elapsed_str = format!("{elapsed}s");
                let display_url = if authorize_url.len() > (width.saturating_sub(4)) {
                    let cap = width.saturating_sub(7);
                    format!("{}...", &authorize_url[..cap.min(authorize_url.len())])
                } else {
                    authorize_url.clone()
                };
                let paste_display = if fallback_url_input.is_empty() {
                    "<paste callback URL or auth code here>".to_string()
                } else {
                    fallback_url_input.clone()
                };
                vec![
                    Line::from(""),
                    Line::from(Span::styled("  Browser opened. Waiting for callback...", normal_style)),
                    Line::from(Span::styled(format!("  Elapsed: {elapsed_str}"), hint_style)),
                    Line::from(""),
                    Line::from(Span::styled("  If browser did not open, visit:", hint_style)),
                    Line::from(Span::styled(format!("  {display_url}"), link_style)),
                    Line::from(""),
                    Line::from(Span::styled("  Or paste the callback URL / auth code:", label_style)),
                    Line::from(Span::styled(
                        format!("  {paste_display}"),
                        if fallback_url_input.is_empty() { hint_style } else { Style::default().fg(Color::Cyan) },
                    )),
                    Line::from(""),
                    Line::from(Span::styled("  Enter: submit pasted code   Esc: cancel", hint_style)),
                ]
            }

            ProviderLoginScreen::MultiField { fields, focused, error, .. } => {
                let mut out = vec![Line::from("")];
                for (i, field) in fields.iter().enumerate() {
                    let is_focused = i == *focused;
                    let label_s = if is_focused {
                        Style::default().fg(accent).add_modifier(Modifier::BOLD)
                    } else {
                        label_style
                    };
                    out.push(Line::from(Span::styled(
                        format!("  {}:", field.label),
                        label_s,
                    )));
                    let val = field.display();
                    let is_placeholder = val.is_empty();
                    let show = if is_placeholder {
                        field.placeholder.to_string()
                    } else {
                        val
                    };
                    let val_style = if is_placeholder {
                        hint_style
                    } else {
                        Style::default().fg(Color::Cyan)
                    };
                    let border = if is_focused { "> " } else { "  " };
                    out.push(Line::from(Span::styled(format!("  {border}{show}"), val_style)));
                    out.push(Line::from(""));
                }
                if let Some(err) = error {
                    out.push(Line::from(Span::styled(format!("  Error: {err}"), error_style)));
                    out.push(Line::from(""));
                }
                out.push(Line::from(Span::styled(
                    "  Tab: next field   Enter: save   Esc: cancel",
                    hint_style,
                )));
                out
            }

            ProviderLoginScreen::Instructions { lines, .. } => {
                let mut out = vec![Line::from("")];
                for l in lines.iter() {
                    out.push(Line::from(Span::styled(l.to_string(), normal_style)));
                }
                out
            }

            ProviderLoginScreen::Result { ok, message, .. } => {
                let icon_style = if *ok {
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                } else {
                    error_style.add_modifier(Modifier::BOLD)
                };
                let icon = if *ok { "  Credentials saved." } else { "  Login failed." };
                vec![
                    Line::from(""),
                    Line::from(Span::styled(icon, icon_style)),
                    Line::from(""),
                    Line::from(Span::styled(format!("  {message}"), normal_style)),
                    Line::from(""),
                    Line::from(Span::styled("  Press Esc or Enter to close.", hint_style)),
                ]
            }
        }
    }

    /// Process a key event.  Returns `ProviderLoginAction` indicating what happened.
    pub fn handle_key(&mut self, key: KeyEvent) -> ProviderLoginAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match &mut self.screen {
            // ── AuthMethodPicker ─────────────────────────────────────────────
            ProviderLoginScreen::AuthMethodPicker {
                provider,
                methods,
                selected,
            } => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        return ProviderLoginAction::Cancel;
                    }
                    KeyCode::Up => {
                        if *selected > 0 {
                            *selected -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if *selected + 1 < methods.len() {
                            *selected += 1;
                        }
                    }
                    KeyCode::Enter => {
                        let choice = *selected;
                        let prov = *provider;
                        let n_methods = methods.len();
                        // For Anthropic: 0 = OAuth, 1 = API key.
                        // For any provider with only one method that's OAuth: 0 = OAuth.
                        if n_methods == 2 && choice == 0 {
                            // OAuth
                            return ProviderLoginAction::StartAnthropicOAuth;
                        } else {
                            // API key — transition screen
                            let (env_var, vault_key, prefix) = api_key_meta_for_kind(prov);
                            self.screen = ProviderLoginScreen::ApiKeyPaste {
                                provider: prov,
                                input: String::new(),
                                cursor: 0,
                                env_var,
                                vault_key,
                                prefix,
                                error: None,
                            };
                        }
                    }
                    _ => {}
                }
                ProviderLoginAction::Continue
            }

            // ── ApiKeyPaste ──────────────────────────────────────────────────
            ProviderLoginScreen::ApiKeyPaste {
                provider,
                input,
                cursor,
                vault_key,
                prefix,
                error,
                ..
            } => {
                if ctrl && matches!(key.code, KeyCode::Char('c')) {
                    return ProviderLoginAction::Cancel;
                }
                match key.code {
                    KeyCode::Esc => return ProviderLoginAction::Cancel,
                    KeyCode::Enter => {
                        let key_str = input.trim().to_string();
                        if key_str.is_empty() {
                            *error = Some("No key provided.".to_string());
                            return ProviderLoginAction::Continue;
                        }
                        let vk = *vault_key;
                        let pfx: &str = prefix;
                        if !pfx.is_empty() && !key_str.starts_with(pfx) {
                            *error = Some(format!(
                                "Warning: key doesn't start with '{pfx}'. Saved anyway."
                            ));
                        }
                        let prov = *provider;
                        return ProviderLoginAction::SaveApiKey {
                            provider: prov,
                            vault_key: vk,
                            key: key_str,
                        };
                    }
                    KeyCode::Char(ch) => {
                        input.insert(*cursor, ch);
                        *cursor += ch.len_utf8();
                    }
                    KeyCode::Backspace => {
                        if *cursor > 0 {
                            let prev = crate::tui::helpers::prev_char_boundary(input, *cursor);
                            input.drain(prev..*cursor);
                            *cursor = prev;
                        }
                    }
                    KeyCode::Delete => {
                        if *cursor < input.len() {
                            let next = crate::tui::helpers::next_char_boundary(input, *cursor);
                            input.drain(*cursor..next);
                        }
                    }
                    KeyCode::Left => {
                        if *cursor > 0 {
                            *cursor = crate::tui::helpers::prev_char_boundary(input, *cursor);
                        }
                    }
                    KeyCode::Right => {
                        if *cursor < input.len() {
                            *cursor = crate::tui::helpers::next_char_boundary(input, *cursor);
                        }
                    }
                    KeyCode::Home => *cursor = 0,
                    KeyCode::End => *cursor = input.len(),
                    _ => {}
                }
                ProviderLoginAction::Continue
            }

            // ── OAuthWaiting ─────────────────────────────────────────────────
            ProviderLoginScreen::OAuthWaiting {
                fallback_url_input,
                fallback_cursor,
                result_rx,
                expected_state,
                pkce_verifier,
                redirect_uri,
                provider,
                ..
            } => {
                if ctrl && matches!(key.code, KeyCode::Char('c')) {
                    return ProviderLoginAction::CancelOAuth;
                }
                match key.code {
                    KeyCode::Esc => return ProviderLoginAction::CancelOAuth,
                    KeyCode::Enter => {
                        // Poll result_rx for automatic callback first.
                        if let Some(rx) = result_rx {
                            if let Ok(outcome) = rx.try_recv() {
                                match outcome {
                                    Ok((code, state)) => {
                                        return ProviderLoginAction::OAuthCodeReceived {
                                            code,
                                            state,
                                            verifier: pkce_verifier.clone(),
                                            redirect_uri: redirect_uri.clone(),
                                        };
                                    }
                                    Err(e) => {
                                        let prov = *provider;
                                        self.screen = ProviderLoginScreen::Result {
                                            provider: prov,
                                            ok: false,
                                            message: e,
                                        };
                                        return ProviderLoginAction::Continue;
                                    }
                                }
                            }
                        }
                        // Try the pasted fallback input.
                        let pasted = fallback_url_input.trim().to_string();
                        if pasted.is_empty() {
                            return ProviderLoginAction::Continue;
                        }
                        match runtime::parse_pasted_oauth_code(&pasted) {
                            Ok((code, state_opt)) => {
                                let state = state_opt.unwrap_or_else(|| expected_state.clone());
                                return ProviderLoginAction::OAuthCodeReceived {
                                    code,
                                    state,
                                    verifier: pkce_verifier.clone(),
                                    redirect_uri: redirect_uri.clone(),
                                };
                            }
                            Err(e) => {
                                let prov = *provider;
                                self.screen = ProviderLoginScreen::Result {
                                    provider: prov,
                                    ok: false,
                                    message: format!("Could not parse pasted value: {e}"),
                                };
                            }
                        }
                        ProviderLoginAction::Continue
                    }
                    KeyCode::Char(ch) => {
                        fallback_url_input.insert(*fallback_cursor, ch);
                        *fallback_cursor += ch.len_utf8();
                        ProviderLoginAction::PollOAuth
                    }
                    KeyCode::Backspace => {
                        if *fallback_cursor > 0 {
                            let prev = crate::tui::helpers::prev_char_boundary(
                                fallback_url_input,
                                *fallback_cursor,
                            );
                            fallback_url_input.drain(prev..*fallback_cursor);
                            *fallback_cursor = prev;
                        }
                        ProviderLoginAction::Continue
                    }
                    _ => ProviderLoginAction::PollOAuth,
                }
            }

            // ── MultiField ───────────────────────────────────────────────────
            ProviderLoginScreen::MultiField {
                provider,
                fields,
                focused,
                error: _error,
            } => {
                if ctrl && matches!(key.code, KeyCode::Char('c')) {
                    return ProviderLoginAction::Cancel;
                }
                match key.code {
                    KeyCode::Esc => return ProviderLoginAction::Cancel,
                    KeyCode::Tab => {
                        *focused = (*focused + 1) % fields.len();
                    }
                    KeyCode::BackTab => {
                        *focused = if *focused == 0 {
                            fields.len() - 1
                        } else {
                            *focused - 1
                        };
                    }
                    KeyCode::Enter => {
                        let prov = *provider;
                        let values: Vec<String> = fields.iter().map(|f| f.value.clone()).collect();
                        return ProviderLoginAction::SaveMultiField {
                            provider: prov,
                            values,
                        };
                    }
                    KeyCode::Char(ch) => {
                        let f = &mut fields[*focused];
                        f.value.insert(f.cursor, ch);
                        f.cursor += ch.len_utf8();
                    }
                    KeyCode::Backspace => {
                        let f = &mut fields[*focused];
                        if f.cursor > 0 {
                            let prev = crate::tui::helpers::prev_char_boundary(&f.value, f.cursor);
                            f.value.drain(prev..f.cursor);
                            f.cursor = prev;
                        }
                    }
                    KeyCode::Delete => {
                        let f = &mut fields[*focused];
                        if f.cursor < f.value.len() {
                            let next = crate::tui::helpers::next_char_boundary(&f.value, f.cursor);
                            f.value.drain(f.cursor..next);
                        }
                    }
                    _ => {}
                }
                ProviderLoginAction::Continue
            }

            // ── Instructions ─────────────────────────────────────────────────
            ProviderLoginScreen::Instructions { .. } => {
                // Any key closes.
                match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                        ProviderLoginAction::Cancel
                    }
                    _ => ProviderLoginAction::Continue,
                }
            }

            // ── Result ───────────────────────────────────────────────────────
            ProviderLoginScreen::Result { .. } => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                    ProviderLoginAction::Dismiss
                }
                _ => ProviderLoginAction::Continue,
            },
        }
    }

    /// Poll the background OAuth listener without blocking.
    /// Returns `Some(ProviderLoginAction)` if a callback arrived.
    pub fn poll_oauth_listener(&mut self) -> Option<ProviderLoginAction> {
        // Extract what we need before the borrow, to avoid borrow-checker issues.
        let outcome = if let ProviderLoginScreen::OAuthWaiting {
            result_rx: Some(rx),
            ..
        } = &mut self.screen
        {
            rx.try_recv().ok()
        } else {
            return None;
        };

        let outcome = outcome?;
        let (verifier, ruri) = if let ProviderLoginScreen::OAuthWaiting {
            pkce_verifier,
            redirect_uri,
            ..
        } = &self.screen
        {
            (pkce_verifier.clone(), redirect_uri.clone())
        } else {
            return None;
        };

        Some(match outcome {
            Ok((code, state)) => ProviderLoginAction::OAuthCodeReceived {
                code,
                state,
                verifier,
                redirect_uri: ruri,
            },
            Err(e) => {
                let provider = if let ProviderLoginScreen::OAuthWaiting { provider, .. } = &self.screen {
                    *provider
                } else {
                    ProviderKind::AnvilApi
                };
                self.screen = ProviderLoginScreen::Result {
                    provider,
                    ok: false,
                    message: e,
                };
                ProviderLoginAction::Continue
            }
        })
    }

    /// Transition to the OAuthWaiting screen after the browser flow has been
    /// started by the caller.
    pub fn set_oauth_waiting(
        &mut self,
        port: Option<u16>,
        authorize_url: String,
        expected_state: String,
        pkce_verifier: String,
        redirect_uri: String,
        result_rx: std::sync::mpsc::Receiver<Result<(String, String), String>>,
    ) {
        let provider = match &self.screen {
            ProviderLoginScreen::AuthMethodPicker { provider, .. } => *provider,
            ProviderLoginScreen::OAuthWaiting { provider, .. } => *provider,
            _ => ProviderKind::AnvilApi,
        };
        self.screen = ProviderLoginScreen::OAuthWaiting {
            provider,
            started_at: Instant::now(),
            port,
            authorize_url,
            fallback_url_input: String::new(),
            fallback_cursor: 0,
            result_rx: Some(result_rx),
            expected_state,
            pkce_verifier,
            redirect_uri,
        };
    }

    /// Transition to the Result screen.
    pub fn set_result(&mut self, ok: bool, message: String) {
        let provider = self.current_provider();
        self.screen = ProviderLoginScreen::Result {
            provider,
            ok,
            message,
        };
    }

    /// Extract the provider from whatever the current screen is.
    fn current_provider(&self) -> ProviderKind {
        match &self.screen {
            ProviderLoginScreen::AuthMethodPicker { provider, .. }
            | ProviderLoginScreen::ApiKeyPaste { provider, .. }
            | ProviderLoginScreen::MultiField { provider, .. }
            | ProviderLoginScreen::Instructions { provider, .. }
            | ProviderLoginScreen::Result { provider, .. } => *provider,
            ProviderLoginScreen::OAuthWaiting { provider, .. } => *provider,
        }
    }

    /// True when the modal should receive key events (not yet closed/dismissed).
    pub fn is_active(&self) -> bool {
        !matches!(&self.screen, ProviderLoginScreen::Result { .. })
    }

    /// Build a `ProviderLoginRenderSnapshot` for the draw closure.
    /// Strips the `mpsc::Receiver` so the snapshot is `Clone`.
    pub fn render_snapshot(&self) -> ProviderLoginRenderSnapshot {
        let screen = match &self.screen {
            ProviderLoginScreen::AuthMethodPicker { provider, methods, selected } => {
                ProviderLoginRenderScreen::AuthMethodPicker {
                    provider: *provider,
                    methods: methods.clone(),
                    selected: *selected,
                }
            }
            ProviderLoginScreen::ApiKeyPaste { provider, input, env_var, prefix, error, .. } => {
                ProviderLoginRenderScreen::ApiKeyPaste {
                    provider: *provider,
                    input: input.clone(),
                    env_var,
                    prefix,
                    error: error.clone(),
                }
            }
            ProviderLoginScreen::OAuthWaiting {
                provider,
                started_at,
                authorize_url,
                fallback_url_input,
                ..
            } => {
                ProviderLoginRenderScreen::OAuthWaiting {
                    provider: *provider,
                    started_at: *started_at,
                    authorize_url: authorize_url.clone(),
                    fallback_url_input: fallback_url_input.clone(),
                }
            }
            ProviderLoginScreen::MultiField { provider, fields, focused, error } => {
                ProviderLoginRenderScreen::MultiField {
                    provider: *provider,
                    fields: fields.clone(),
                    focused: *focused,
                    error: error.clone(),
                }
            }
            ProviderLoginScreen::Instructions { provider, lines } => {
                ProviderLoginRenderScreen::Instructions {
                    provider: *provider,
                    lines: lines.clone(),
                }
            }
            ProviderLoginScreen::Result { provider, ok, message } => {
                ProviderLoginRenderScreen::Result {
                    provider: *provider,
                    ok: *ok,
                    message: message.clone(),
                }
            }
        };
        ProviderLoginRenderSnapshot { screen }
    }
}

// ─── ProviderLoginAction ─────────────────────────────────────────────────────

/// What the modal is asking the host (AnvilTui / LiveCli) to do.
#[derive(Debug)]
pub enum ProviderLoginAction {
    /// Nothing to act on; redraw and continue.
    Continue,
    /// User dismissed the modal.
    Cancel,
    /// User dismissed the result screen.
    Dismiss,
    /// User wants to cancel an OAuth flow in progress (signal the listener thread).
    CancelOAuth,
    /// Background OAuth listener should be polled for an incoming callback.
    PollOAuth,
    /// Start Anthropic OAuth browser flow.
    StartAnthropicOAuth,
    /// Token exchange is complete: the given code+state were received.
    OAuthCodeReceived {
        code: String,
        state: String,
        verifier: String,
        redirect_uri: String,
    },
    /// Save a single API key credential.
    SaveApiKey {
        provider: ProviderKind,
        vault_key: &'static str,
        key: String,
    },
    /// Save multi-field credentials (Ollama, etc.).
    SaveMultiField {
        provider: ProviderKind,
        values: Vec<String>,
    },
}

// ─── Render snapshot implementation ──────────────────────────────────────────

fn render_snapshot_impl(
    snap: &ProviderLoginRenderSnapshot,
    frame: &mut Frame,
    area: Rect,
    theme_accent: Color,
    theme_error: Color,
) {
    // Determine the title and border color based on screen type.
    let (title, border_color, preferred_h) = match &snap.screen {
        ProviderLoginRenderScreen::AuthMethodPicker { provider, methods, .. } => (
            format!(" {} Login ", provider_display(provider)),
            theme_accent,
            (6 + methods.len()) as u16,
        ),
        ProviderLoginRenderScreen::ApiKeyPaste { provider, .. } => (
            format!(" {} API Key ", provider_display(provider)),
            theme_accent,
            12u16,
        ),
        ProviderLoginRenderScreen::OAuthWaiting { provider, .. } => (
            format!(" {} OAuth ", provider_display(provider)),
            theme_accent,
            14u16,
        ),
        ProviderLoginRenderScreen::MultiField { provider, fields, .. } => (
            format!(" {} Setup ", provider_display(provider)),
            theme_accent,
            (8 + fields.len() * 3) as u16,
        ),
        ProviderLoginRenderScreen::Instructions { provider, lines } => (
            format!(" {} Setup ", provider_display(provider)),
            theme_accent,
            (4 + lines.len()) as u16,
        ),
        ProviderLoginRenderScreen::Result { provider, ok, .. } => {
            let t = if *ok {
                format!(" {} Login Complete ", provider_display(provider))
            } else {
                format!(" {} Login Failed ", provider_display(provider))
            };
            let bc = if *ok { theme_accent } else { theme_error };
            (t, bc, 8u16)
        }
    };

    let modal_w = (area.width.saturating_sub(8)).min(70);
    let modal_h = preferred_h.min(area.height.saturating_sub(4));
    let modal_x = (area.width.saturating_sub(modal_w)) / 2;
    let modal_y = (area.height.saturating_sub(modal_h)) / 2;
    let modal_area = Rect {
        x: modal_x,
        y: modal_y,
        width: modal_w,
        height: modal_h,
    };

    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color).add_modifier(Modifier::BOLD))
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    let lines = build_render_lines(&snap.screen, inner.width as usize, theme_accent, theme_error);
    let para = Paragraph::new(Text::from(lines));
    frame.render_widget(para, inner);
}

fn build_render_lines(
    screen: &ProviderLoginRenderScreen,
    width: usize,
    accent: Color,
    error_color: Color,
) -> Vec<Line<'static>> {
    let label_style = Style::default().fg(Color::DarkGray);
    let selected_style = Style::default()
        .fg(Color::Black)
        .bg(accent)
        .add_modifier(Modifier::BOLD);
    let normal_style = Style::default().fg(Color::White);
    let error_style = Style::default().fg(error_color);
    let hint_style = Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM);
    let link_style = Style::default().fg(accent).add_modifier(Modifier::UNDERLINED);
    let cyan_style = Style::default().fg(Color::Cyan);

    match screen {
        ProviderLoginRenderScreen::AuthMethodPicker { methods, selected, .. } => {
            let mut out = vec![
                Line::from(""),
                Line::from(Span::styled("  Select login method:", label_style)),
            ];
            for (i, method) in methods.iter().enumerate() {
                let (prefix, style) = if i == *selected {
                    ("  > ", selected_style)
                } else {
                    ("    ", normal_style)
                };
                out.push(Line::from(Span::styled(format!("{prefix}{method}"), style)));
            }
            out.push(Line::from(""));
            out.push(Line::from(Span::styled(
                "  Up/Down: select   Enter: confirm   Esc: cancel",
                hint_style,
            )));
            out
        }

        ProviderLoginRenderScreen::ApiKeyPaste { env_var, prefix, input, error, .. } => {
            let display: String = if input.is_empty() {
                String::new()
            } else {
                let visible = 4.min(input.len());
                let hidden = input.len().saturating_sub(visible);
                format!("{}{}", &input[..visible], "*".repeat(hidden))
            };
            let mut out = vec![
                Line::from(""),
                Line::from(Span::styled(format!("  Env var: {env_var}"), label_style)),
                Line::from(Span::styled(
                    format!("  Prefix:  {}", if prefix.is_empty() { "(none)" } else { prefix }),
                    hint_style,
                )),
                Line::from(""),
                Line::from(Span::styled("  API Key:", label_style)),
                Line::from(Span::styled(
                    format!(
                        "  {}",
                        if display.is_empty() { "<paste key here>".to_string() } else { display.clone() }
                    ),
                    if display.is_empty() { hint_style } else { cyan_style },
                )),
                Line::from(""),
            ];
            if let Some(err) = error {
                out.push(Line::from(Span::styled(format!("  Error: {err}"), error_style)));
                out.push(Line::from(""));
            }
            out.push(Line::from(Span::styled(
                "  Paste key + Enter to save   Esc: cancel",
                hint_style,
            )));
            out
        }

        ProviderLoginRenderScreen::OAuthWaiting {
            started_at,
            authorize_url,
            fallback_url_input,
            ..
        } => {
            let elapsed = started_at.elapsed().as_secs();
            let display_url = if authorize_url.len() > width.saturating_sub(4) {
                let cap = width.saturating_sub(7);
                format!("{}...", &authorize_url[..cap.min(authorize_url.len())])
            } else {
                authorize_url.clone()
            };
            let paste_display = if fallback_url_input.is_empty() {
                "<paste callback URL or auth code here>".to_string()
            } else {
                fallback_url_input.clone()
            };
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Browser opened. Waiting for callback...",
                    normal_style,
                )),
                Line::from(Span::styled(format!("  Elapsed: {elapsed}s"), hint_style)),
                Line::from(""),
                Line::from(Span::styled("  If browser did not open, visit:", hint_style)),
                Line::from(Span::styled(format!("  {display_url}"), link_style)),
                Line::from(""),
                Line::from(Span::styled("  Or paste callback URL / auth code:", label_style)),
                Line::from(Span::styled(
                    format!("  {paste_display}"),
                    if fallback_url_input.is_empty() { hint_style } else { cyan_style },
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  Enter: submit pasted code   Esc: cancel",
                    hint_style,
                )),
            ]
        }

        ProviderLoginRenderScreen::MultiField { fields, focused, error, .. } => {
            let mut out = vec![Line::from("")];
            for (i, field) in fields.iter().enumerate() {
                let is_focused = i == *focused;
                let label_s = if is_focused {
                    Style::default().fg(accent).add_modifier(Modifier::BOLD)
                } else {
                    label_style
                };
                out.push(Line::from(Span::styled(
                    format!("  {}:", field.label),
                    label_s,
                )));
                let val = field.display();
                let is_placeholder = val.is_empty();
                let show = if is_placeholder {
                    field.placeholder.to_string()
                } else {
                    val
                };
                let val_style = if is_placeholder {
                    hint_style
                } else {
                    cyan_style
                };
                let border = if is_focused { "> " } else { "  " };
                out.push(Line::from(Span::styled(format!("  {border}{show}"), val_style)));
                out.push(Line::from(""));
            }
            if let Some(err) = error {
                out.push(Line::from(Span::styled(format!("  Error: {err}"), error_style)));
                out.push(Line::from(""));
            }
            out.push(Line::from(Span::styled(
                "  Tab: next field   Enter: save   Esc: cancel",
                hint_style,
            )));
            out
        }

        ProviderLoginRenderScreen::Instructions { lines, .. } => {
            let mut out = vec![Line::from("")];
            for l in lines.iter() {
                out.push(Line::from(Span::styled(l.to_string(), normal_style)));
            }
            out
        }

        ProviderLoginRenderScreen::Result { ok, message, .. } => {
            let icon_style = if *ok {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else {
                error_style.add_modifier(Modifier::BOLD)
            };
            let icon = if *ok { "  Credentials saved." } else { "  Login failed." };
            vec![
                Line::from(""),
                Line::from(Span::styled(icon, icon_style)),
                Line::from(""),
                Line::from(Span::styled(format!("  {message}"), normal_style)),
                Line::from(""),
                Line::from(Span::styled("  Press Esc or Enter to close.", hint_style)),
            ]
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn provider_display(kind: &ProviderKind) -> &'static str {
    match kind {
        ProviderKind::AnvilApi => "Anthropic",
        ProviderKind::OpenAi => "OpenAI",
        ProviderKind::Gemini => "Gemini",
        ProviderKind::Ollama => "Ollama",
        ProviderKind::Xai => "xAI",
        ProviderKind::Fireworks => "Fireworks",
        ProviderKind::MiniMax => "MiniMax",
        ProviderKind::Groq => "Groq",
        ProviderKind::Mistral => "Mistral",
        ProviderKind::Perplexity => "Perplexity",
        ProviderKind::DeepSeek => "DeepSeek",
        ProviderKind::TogetherAi => "Together AI",
        ProviderKind::DeepInfra => "DeepInfra",
        ProviderKind::Chutes => "Chutes",
        ProviderKind::Cerebras => "Cerebras",
        ProviderKind::NvidiaNim => "NVIDIA NIM",
        ProviderKind::HuggingFace => "Hugging Face",
        ProviderKind::MoonshotAi => "Moonshot AI",
        ProviderKind::Nebius => "Nebius",
        ProviderKind::Scaleway => "Scaleway",
        ProviderKind::StackIt => "STACKIT",
        ProviderKind::Baseten => "Baseten",
        ProviderKind::Cortecs => "Cortecs",
        ProviderKind::Ai302 => "302.ai",
        ProviderKind::Zai => "Zai",
        ProviderKind::OpenRouter => "OpenRouter",
        ProviderKind::LmStudio => "LM Studio",
        ProviderKind::OpenCode => "OpenCode",
        ProviderKind::OpenCodeGo => "OpenCode Go",
        ProviderKind::Copilot => "GitHub Copilot",
        ProviderKind::Azure => "Azure OpenAI",
        ProviderKind::Bedrock => "AWS Bedrock",
        ProviderKind::Alibaba => "Alibaba DashScope",
        ProviderKind::Antigravity => "Antigravity",
        ProviderKind::Cursor => "Cursor",
    }
}

/// Return `(env_var, vault_key, prefix)` for an API-key-based provider.
pub fn api_key_meta_for_kind(kind: ProviderKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        ProviderKind::AnvilApi => ("ANTHROPIC_API_KEY", "anthropic_api_key", "sk-ant-"),
        ProviderKind::OpenAi => ("OPENAI_API_KEY", "openai_api_key", "sk-"),
        ProviderKind::Gemini => ("GEMINI_API_KEY", "gemini_api_key", "AIza"),
        ProviderKind::Xai => ("XAI_API_KEY", "xai_api_key", "xai-"),
        ProviderKind::Fireworks => ("FIREWORKS_API_KEY", "fireworks_api_key", "fw-"),
        ProviderKind::MiniMax => ("MINIMAX_API_KEY", "minimax_api_key", ""),
        ProviderKind::Groq => ("GROQ_API_KEY", "groq_api_key", "gsk_"),
        ProviderKind::Mistral => ("MISTRAL_API_KEY", "mistral_api_key", ""),
        ProviderKind::Perplexity => ("PPLX_API_KEY", "perplexity_api_key", "pplx-"),
        ProviderKind::DeepSeek => ("DEEPSEEK_API_KEY", "deepseek_api_key", "sk-"),
        ProviderKind::TogetherAi => ("TOGETHER_API_KEY", "together_api_key", ""),
        ProviderKind::DeepInfra => ("DEEPINFRA_API_KEY", "deepinfra_api_key", ""),
        ProviderKind::Chutes => ("CHUTES_API_KEY", "chutes_api_key", ""),
        ProviderKind::Cerebras => ("CEREBRAS_API_KEY", "cerebras_api_key", "csk-"),
        ProviderKind::NvidiaNim => ("NVIDIA_API_KEY", "nvidia_api_key", "nvapi-"),
        ProviderKind::HuggingFace => ("HF_TOKEN", "huggingface_token", "hf_"),
        ProviderKind::MoonshotAi => ("MOONSHOT_API_KEY", "moonshot_api_key", "sk-"),
        ProviderKind::Nebius => ("NEBIUS_API_KEY", "nebius_api_key", ""),
        ProviderKind::Scaleway => ("SCALEWAY_API_KEY", "scaleway_api_key", ""),
        ProviderKind::StackIt => ("STACKIT_API_KEY", "stackit_api_key", ""),
        ProviderKind::Baseten => ("BASETEN_API_KEY", "baseten_api_key", ""),
        ProviderKind::Cortecs => ("CORTECS_API_KEY", "cortecs_api_key", ""),
        ProviderKind::Ai302 => ("AI302_API_KEY", "ai302_api_key", ""),
        ProviderKind::Zai => ("ZAI_API_KEY", "zai_api_key", ""),
        ProviderKind::OpenRouter => ("OPENROUTER_API_KEY", "openrouter_api_key", "sk-or-"),
        ProviderKind::LmStudio => ("LMSTUDIO_API_KEY", "lmstudio_api_key", ""),
        ProviderKind::OpenCode => ("OPENCODE_API_KEY", "opencode_api_key", ""),
        ProviderKind::OpenCodeGo => ("OPENCODE_GO_API_KEY", "opencode_go_api_key", ""),
        ProviderKind::Alibaba => ("DASHSCOPE_API_KEY", "dashscope_api_key", "sk-"),
        ProviderKind::Antigravity => ("ANTIGRAVITY_API_KEY", "antigravity_api_key", ""),
        ProviderKind::Cursor => ("CURSOR_API_KEY", "cursor_api_key", ""),
        // Non-API-key providers fall through to empty strings.
        _ => ("", "", ""),
    }
}

/// Build an `ApiKeyPaste` screen for any simple API-key provider.
fn api_key_screen_for_kind(provider: ProviderKind) -> ProviderLoginScreen {
    let (env_var, vault_key, prefix) = api_key_meta_for_kind(provider);
    ProviderLoginScreen::ApiKeyPaste {
        provider,
        input: String::new(),
        cursor: 0,
        env_var,
        vault_key,
        prefix,
        error: None,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_opens_to_picker_for_anthropic() {
        let modal = ProviderLoginModal::open(ProviderKind::AnvilApi);
        assert!(
            matches!(modal.screen, ProviderLoginScreen::AuthMethodPicker { .. }),
            "Anthropic should open to picker"
        );
    }

    #[test]
    fn modal_opens_to_api_key_for_openai() {
        let modal = ProviderLoginModal::open(ProviderKind::OpenAi);
        assert!(
            matches!(modal.screen, ProviderLoginScreen::ApiKeyPaste { .. }),
            "OpenAI should open directly to ApiKeyPaste"
        );
    }

    #[test]
    fn modal_opens_to_multi_field_for_ollama() {
        let modal = ProviderLoginModal::open(ProviderKind::Ollama);
        assert!(
            matches!(modal.screen, ProviderLoginScreen::MultiField { .. }),
            "Ollama should open to MultiField"
        );
    }

    #[test]
    fn modal_opens_to_instructions_for_azure() {
        let modal = ProviderLoginModal::open(ProviderKind::Azure);
        assert!(
            matches!(modal.screen, ProviderLoginScreen::Instructions { .. }),
            "Azure should open to Instructions"
        );
    }

    #[test]
    fn esc_on_picker_cancels() {
        let mut modal = ProviderLoginModal::open(ProviderKind::AnvilApi);
        let action = modal.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(action, ProviderLoginAction::Cancel));
    }

    #[test]
    fn esc_on_api_key_paste_cancels() {
        let mut modal = ProviderLoginModal::open(ProviderKind::OpenAi);
        let action = modal.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(action, ProviderLoginAction::Cancel));
    }

    #[test]
    fn picker_down_up_navigation() {
        let mut modal = ProviderLoginModal::open(ProviderKind::AnvilApi);
        // Move down
        modal.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        if let ProviderLoginScreen::AuthMethodPicker { selected, .. } = &modal.screen {
            assert_eq!(*selected, 1);
        } else {
            panic!("expected AuthMethodPicker");
        }
        // Move up
        modal.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        if let ProviderLoginScreen::AuthMethodPicker { selected, .. } = &modal.screen {
            assert_eq!(*selected, 0);
        }
    }

    #[test]
    fn picker_enter_on_api_key_transitions_to_paste() {
        let mut modal = ProviderLoginModal::open(ProviderKind::AnvilApi);
        // Select API key (index 1)
        modal.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        modal.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(modal.screen, ProviderLoginScreen::ApiKeyPaste { .. }),
            "Enter on API key row should transition to ApiKeyPaste"
        );
    }

    #[test]
    fn api_key_paste_enter_with_key_emits_save_action() {
        let mut modal = ProviderLoginModal::open(ProviderKind::OpenAi);
        // Type a fake key
        for ch in "sk-fakekey123".chars() {
            modal.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = modal.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(action, ProviderLoginAction::SaveApiKey { .. }),
            "Enter with typed key should emit SaveApiKey"
        );
    }

    #[test]
    fn api_key_paste_enter_empty_sets_error() {
        let mut modal = ProviderLoginModal::open(ProviderKind::OpenAi);
        modal.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        if let ProviderLoginScreen::ApiKeyPaste { error, .. } = &modal.screen {
            assert!(error.is_some(), "Empty paste should set error");
        }
    }

    #[test]
    fn esc_on_oauth_waiting_cancels_oauth() {
        let mut modal = ProviderLoginModal::open(ProviderKind::AnvilApi);
        // Manually put it into OAuthWaiting
        let (tx, rx) = std::sync::mpsc::channel();
        drop(tx); // no sender, so receiver immediately disconnects
        modal.screen = ProviderLoginScreen::OAuthWaiting {
            provider: ProviderKind::AnvilApi,
            started_at: Instant::now(),
            port: None,
            authorize_url: "https://example.com".to_string(),
            fallback_url_input: String::new(),
            fallback_cursor: 0,
            result_rx: Some(rx),
            expected_state: "state-xyz".to_string(),
            pkce_verifier: "verifier".to_string(),
            redirect_uri: "http://localhost:9876/callback".to_string(),
        };
        let action = modal.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(action, ProviderLoginAction::CancelOAuth));
    }

    #[test]
    fn oauth_waiting_paste_then_enter_parses_callback_url() {
        let mut modal = ProviderLoginModal::open(ProviderKind::AnvilApi);
        let (tx, rx) = std::sync::mpsc::channel::<Result<(String, String), String>>();
        drop(tx);
        modal.screen = ProviderLoginScreen::OAuthWaiting {
            provider: ProviderKind::AnvilApi,
            started_at: Instant::now(),
            port: None,
            authorize_url: String::new(),
            fallback_url_input: String::new(),
            fallback_cursor: 0,
            result_rx: Some(rx),
            expected_state: "xyzstate".to_string(),
            pkce_verifier: "myverifier".to_string(),
            redirect_uri: "http://localhost:9876/callback".to_string(),
        };

        // Type a full callback URL into the paste box
        let url = "http://localhost:9876/callback?code=mycode123&state=xyzstate";
        for ch in url.chars() {
            modal.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = modal.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            ProviderLoginAction::OAuthCodeReceived { code, state, .. } => {
                assert_eq!(code, "mycode123");
                assert_eq!(state, "xyzstate");
            }
            other => panic!("Expected OAuthCodeReceived, got {other:?}"),
        }
    }

    #[test]
    fn set_result_transitions_to_result_screen() {
        let mut modal = ProviderLoginModal::open(ProviderKind::OpenAi);
        modal.set_result(true, "OpenAI API key saved.".to_string());
        assert!(
            matches!(modal.screen, ProviderLoginScreen::Result { ok: true, .. }),
            "set_result(true) should show Result screen"
        );
    }

    #[test]
    fn is_active_false_on_result_screen() {
        let mut modal = ProviderLoginModal::open(ProviderKind::OpenAi);
        modal.set_result(false, "error".to_string());
        assert!(!modal.is_active(), "modal should not be active on Result screen");
    }
}
