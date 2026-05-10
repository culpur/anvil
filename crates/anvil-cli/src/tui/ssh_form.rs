//! Modal SSH connection form (T5-Ssh-E).
//!
//! Renders a floating dialog centered over the TUI for the user to fill in
//! connection details.  The form validates on submit and returns either a
//! ready-to-use `SshConfig` or a `Cancelled` signal.
//!
//! ## Keyboard navigation
//! - Tab / Shift+Tab   move focus forward / backward through visible fields
//! - Enter             advance focus (Submit field: validate + close)
//! - Esc               cancel and close the form
//! - Left/Right or Home/End  text editing within a field
//! - Backspace / Delete      character deletion within a field
//! - Up/Down           cycle auth-method selector

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use runtime::ssh::SshConfig;
use runtime::ssh::SshAuthMethod;

// ─── Enums ───────────────────────────────────────────────────────────────────

/// Which field holds keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshFormField {
    Host,
    Port,
    User,
    AuthMethod,
    KeyPath,
    Secret,
    Alias,
    Submit,
}

impl SshFormField {
    /// All fields in display order (including conditional ones).
    const ALL: &'static [SshFormField] = &[
        SshFormField::Host,
        SshFormField::Port,
        SshFormField::User,
        SshFormField::AuthMethod,
        SshFormField::KeyPath,
        SshFormField::Secret,
        SshFormField::Alias,
        SshFormField::Submit,
    ];
}

/// Auth-method index → label / variant mapping.
const AUTH_LABELS: &[&str] = &["Agent", "Key file", "Password"];
const AUTH_IDX_AGENT: usize = 0;
const AUTH_IDX_KEY: usize = 1;
const AUTH_IDX_PASSWORD: usize = 2;

/// The outcome of a form interaction.
pub enum SshFormResult {
    /// User pressed Esc or /ssh cancel. The form should be closed.
    Cancelled,
    /// User submitted the form. Contains the ready config and an optional alias
    /// name to save to the vault.
    Submit(SshConfig, Option<String>),
}

// ─── State ───────────────────────────────────────────────────────────────────

/// All mutable state for the SSH connection form.
pub struct SshFormState {
    pub host: String,
    pub port_str: String,
    pub user: String,
    /// 0 = Agent, 1 = Key file, 2 = Password
    pub auth_index: usize,
    /// Key-file path (visible only when auth_index == KEY).
    pub key_path: String,
    /// Password or key passphrase (masked with '*'; visible only when auth_index != Agent).
    pub secret: String,
    /// Optional alias name to save after a successful connect.
    pub alias: String,
    /// Which field currently has focus.
    pub focused: SshFormField,
    /// Validation error message shown at the bottom of the form.
    pub error: Option<String>,
}

impl Default for SshFormState {
    fn default() -> Self {
        Self::new()
    }
}

impl SshFormState {
    /// Create a blank form with port defaulting to "22" and Agent auth.
    pub fn new() -> Self {
        Self {
            host: String::new(),
            port_str: "22".to_string(),
            user: String::new(),
            auth_index: AUTH_IDX_AGENT,
            key_path: String::new(),
            secret: String::new(),
            alias: String::new(),
            focused: SshFormField::Host,
            error: None,
        }
    }

    /// Pre-fill the form from an `SshConfig` + optional alias name.
    /// Used by `/ssh <alias>` to load a saved connection for editing.
    pub fn prefill(&mut self, config: &SshConfig, alias: Option<&str>) {
        self.host = config.host.clone();
        self.port_str = config.port.to_string();
        self.user = config.user.clone();
        match &config.auth {
            SshAuthMethod::Agent => {
                self.auth_index = AUTH_IDX_AGENT;
                self.key_path = String::new();
                self.secret = String::new();
            }
            SshAuthMethod::KeyFile { path, passphrase } => {
                self.auth_index = AUTH_IDX_KEY;
                self.key_path = path.to_string_lossy().into_owned();
                self.secret = passphrase.clone().unwrap_or_default();
            }
            SshAuthMethod::Password(pw) => {
                self.auth_index = AUTH_IDX_PASSWORD;
                self.key_path = String::new();
                self.secret = pw.clone();
            }
            SshAuthMethod::KeyboardInteractive => {
                // Treat as password-style in the UI; no secret prefilled.
                self.auth_index = AUTH_IDX_PASSWORD;
                self.key_path = String::new();
                self.secret = String::new();
            }
        }
        if let Some(a) = alias {
            self.alias = a.to_string();
        }
    }

    /// Whether the KeyPath field is visible given the current auth method.
    fn key_path_visible(&self) -> bool {
        self.auth_index == AUTH_IDX_KEY
    }

    /// Whether the Secret field is visible given the current auth method.
    fn secret_visible(&self) -> bool {
        self.auth_index != AUTH_IDX_AGENT
    }

    /// Ordered list of *visible* fields used for Tab navigation.
    fn visible_fields(&self) -> Vec<SshFormField> {
        SshFormField::ALL
            .iter()
            .copied()
            .filter(|f| match f {
                SshFormField::KeyPath => self.key_path_visible(),
                SshFormField::Secret => self.secret_visible(),
                _ => true,
            })
            .collect()
    }

    /// Advance focus to the next visible field (wraps from Submit → Host).
    pub fn tab(&mut self) {
        let visible = self.visible_fields();
        if let Some(pos) = visible.iter().position(|&f| f == self.focused) {
            self.focused = visible[(pos + 1) % visible.len()];
        } else {
            self.focused = visible.first().copied().unwrap_or(SshFormField::Host);
        }
        self.error = None;
    }

    /// Move focus to the previous visible field (wraps from Host → Submit).
    pub fn back_tab(&mut self) {
        let visible = self.visible_fields();
        if let Some(pos) = visible.iter().position(|&f| f == self.focused) {
            self.focused = visible[(pos + visible.len() - 1) % visible.len()];
        } else {
            self.focused = visible.last().copied().unwrap_or(SshFormField::Submit);
        }
        self.error = None;
    }

    /// Validate all fields and return `Ok(SshConfig)` or `Err(message)`.
    pub fn validate(&self) -> Result<SshConfig, String> {
        let host = self.host.trim().to_string();
        if host.is_empty() {
            return Err("Host is required".into());
        }

        let port: u16 = self
            .port_str
            .trim()
            .parse()
            .map_err(|_| "Port must be a number between 1 and 65535".to_string())?;
        if port == 0 {
            return Err("Port must be greater than 0".into());
        }

        let user = self.user.trim().to_string();
        if user.is_empty() {
            return Err("User is required".into());
        }

        let auth = match self.auth_index {
            AUTH_IDX_AGENT => SshAuthMethod::Agent,
            AUTH_IDX_KEY => {
                let path = self.key_path.trim().to_string();
                if path.is_empty() {
                    return Err("Key file path is required".into());
                }
                let passphrase = if self.secret.is_empty() {
                    None
                } else {
                    Some(self.secret.clone())
                };
                SshAuthMethod::KeyFile {
                    path: PathBuf::from(path),
                    passphrase,
                }
            }
            AUTH_IDX_PASSWORD | _ => {
                let pw = self.secret.clone();
                if pw.is_empty() {
                    return Err("Password is required".into());
                }
                SshAuthMethod::Password(pw)
            }
        };

        Ok(SshConfig { host, port, user, auth })
    }

    /// Process a single keypress and return `Some(result)` if the form should close.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<SshFormResult> {
        match key.code {
            // Cancel
            KeyCode::Esc => {
                return Some(SshFormResult::Cancelled);
            }

            // Navigation
            KeyCode::Tab => {
                if self.focused == SshFormField::Submit {
                    // Tab on Submit = validate + submit
                    return self.try_submit();
                }
                self.tab();
            }
            KeyCode::BackTab => {
                self.back_tab();
            }
            KeyCode::Enter => {
                if self.focused == SshFormField::Submit {
                    return self.try_submit();
                }
                // Enter on any other field advances to the next.
                self.tab();
            }

            // Auth-method cycling (Up/Down on AuthMethod field)
            KeyCode::Up if self.focused == SshFormField::AuthMethod => {
                self.auth_index = (self.auth_index + AUTH_LABELS.len() - 1) % AUTH_LABELS.len();
                self.ensure_focus_visible();
            }
            KeyCode::Down if self.focused == SshFormField::AuthMethod => {
                self.auth_index = (self.auth_index + 1) % AUTH_LABELS.len();
                self.ensure_focus_visible();
            }

            // Text editing on text fields
            KeyCode::Char(c) => {
                if let Some(buf) = self.focused_buf_mut() {
                    buf.push(c);
                    self.error = None;
                }
            }
            KeyCode::Backspace => {
                if let Some(buf) = self.focused_buf_mut() {
                    buf.pop();
                    self.error = None;
                }
            }
            KeyCode::Delete => {
                // Same as backspace for single-char position tracking; a full
                // cursor is not maintained here — the form is intentionally
                // simple (append-only editing, backspace to remove).
                if let Some(buf) = self.focused_buf_mut() {
                    buf.pop();
                    self.error = None;
                }
            }

            _ => {}
        }
        None
    }

    /// Attempt to validate and submit. Sets `self.error` on failure.
    fn try_submit(&mut self) -> Option<SshFormResult> {
        match self.validate() {
            Ok(config) => {
                let alias = if self.alias.trim().is_empty() {
                    None
                } else {
                    Some(self.alias.trim().to_string())
                };
                Some(SshFormResult::Submit(config, alias))
            }
            Err(msg) => {
                self.error = Some(msg);
                None
            }
        }
    }

    /// Return a mutable reference to the string buffer for the focused text
    /// field.  Returns `None` for non-text fields (AuthMethod, Submit).
    fn focused_buf_mut(&mut self) -> Option<&mut String> {
        match self.focused {
            SshFormField::Host => Some(&mut self.host),
            SshFormField::Port => Some(&mut self.port_str),
            SshFormField::User => Some(&mut self.user),
            SshFormField::KeyPath => Some(&mut self.key_path),
            SshFormField::Secret => Some(&mut self.secret),
            SshFormField::Alias => Some(&mut self.alias),
            SshFormField::AuthMethod | SshFormField::Submit => None,
        }
    }

    /// If the focused field was made invisible by an auth-method change, move
    /// focus back to AuthMethod so the user doesn't get stuck.
    fn ensure_focus_visible(&mut self) {
        let visible = self.visible_fields();
        if !visible.contains(&self.focused) {
            self.focused = SshFormField::AuthMethod;
        }
    }

    // ─── Rendering ────────────────────────────────────────────────────────────

    /// Render the form as a floating modal.  `area` is the full terminal area;
    /// the modal is centered at 40% width, minimum 52 columns, 18+ rows tall.
    ///
    /// Call this from inside the `terminal.draw()` closure.
    pub fn render(&self, frame: &mut ratatui::Frame, area: Rect) {
        // Compute modal dimensions.
        let modal_w = ((area.width as u32 * 40 / 100).max(52)).min(area.width as u32) as u16;
        // Row count: title(1) + border(1) + Host+Port+User+Auth+...+Alias+Submit + error(0/1) + border(1)
        // Fields: 7 text + 1 selector + 1 submit = 9 content rows + label lines.
        // Each field = 1 label + 1 input row = 2 lines; plus 1 gap line → aim for 22 rows total.
        let modal_h: u16 = 22;
        let x = area.x + (area.width.saturating_sub(modal_w)) / 2;
        let y = area.y + (area.height.saturating_sub(modal_h)) / 2;
        let modal_area = Rect { x, y, width: modal_w, height: modal_h };

        // Clear the modal area first so underlying content doesn't bleed through.
        frame.render_widget(Clear, modal_area);

        let block = Block::default()
            .title(" SSH Connect ")
            .title_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(Color::Black));
        let inner = block.inner(modal_area);
        frame.render_widget(block, modal_area);

        // Build content lines.
        let mut lines: Vec<Line<'static>> = Vec::new();

        let focused = self.focused;
        let auth_label = AUTH_LABELS[self.auth_index];

        // Helper: render a labelled text row.
        let make_label = |label: &str, active: bool| -> Line<'static> {
            let style = if active {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Line::from(Span::styled(label.to_string(), style))
        };
        let make_input =
            |value: &str, active: bool, masked: bool| -> Line<'static> {
                let display: String = if masked {
                    "*".repeat(value.len())
                } else {
                    value.to_string()
                };
                let text = if active {
                    format!(" {display}_ ")
                } else {
                    format!(" {display}  ")
                };
                let style = if active {
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                Line::from(Span::styled(text, style))
            };

        // Host
        lines.push(make_label("  Host:", focused == SshFormField::Host));
        lines.push(make_input(&self.host, focused == SshFormField::Host, false));

        // Port
        lines.push(make_label("  Port:", focused == SshFormField::Port));
        lines.push(make_input(&self.port_str, focused == SshFormField::Port, false));

        // User
        lines.push(make_label("  User:", focused == SshFormField::User));
        lines.push(make_input(&self.user, focused == SshFormField::User, false));

        // Auth method (selector, not text)
        let auth_active = focused == SshFormField::AuthMethod;
        lines.push(make_label("  Auth:", auth_active));
        {
            let label_text = format!(" < {auth_label} > ");
            let style = if auth_active {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(Span::styled(label_text, style)));
        }

        // Key path (only when KeyFile selected)
        if self.key_path_visible() {
            lines.push(make_label("  Key path:", focused == SshFormField::KeyPath));
            lines.push(make_input(&self.key_path, focused == SshFormField::KeyPath, false));
        }

        // Secret (password or passphrase; hidden when Agent selected)
        if self.secret_visible() {
            let secret_label = if self.auth_index == AUTH_IDX_KEY {
                "  Passphrase (leave blank if none):"
            } else {
                "  Password:"
            };
            lines.push(make_label(secret_label, focused == SshFormField::Secret));
            lines.push(make_input(&self.secret, focused == SshFormField::Secret, true));
        }

        // Alias
        lines.push(make_label("  Save as alias (optional):", focused == SshFormField::Alias));
        lines.push(make_input(&self.alias, focused == SshFormField::Alias, false));

        // Submit button
        {
            let submit_style = if focused == SshFormField::Submit {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Cyan)
            };
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled("  [ Connect ]  ", submit_style)));
        }

        // Error line (if any)
        if let Some(ref err) = self.error {
            lines.push(Line::from(Span::styled(
                format!("  {err}"),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )));
        }

        // Hint line
        lines.push(Line::from(Span::styled(
            "  Tab/Enter=next  Shift+Tab=prev  Esc=cancel",
            Style::default().fg(Color::DarkGray),
        )));

        let para = Paragraph::new(lines);
        frame.render_widget(para, inner);
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn key_char(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn shift_tab() -> KeyEvent {
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    fn filled_form() -> SshFormState {
        let mut f = SshFormState::new();
        f.host = "example.com".into();
        f.user = "admin".into();
        f.port_str = "22".into();
        f.auth_index = AUTH_IDX_AGENT;
        f
    }

    // ── Test 1 ───────────────────────────────────────────────────────────────

    #[test]
    fn new_form_defaults() {
        let f = SshFormState::new();
        assert_eq!(f.port_str, "22");
        assert_eq!(f.auth_index, AUTH_IDX_AGENT);
        assert_eq!(f.focused, SshFormField::Host);
        assert!(f.error.is_none());
    }

    // ── Test 2 ───────────────────────────────────────────────────────────────

    #[test]
    fn tab_cycles_through_visible_fields() {
        let mut f = SshFormState::new();
        // Agent selected — KeyPath should NOT appear.
        assert_eq!(f.focused, SshFormField::Host);
        f.tab(); // Port
        assert_eq!(f.focused, SshFormField::Port);
        f.tab(); // User
        assert_eq!(f.focused, SshFormField::User);
        f.tab(); // AuthMethod
        assert_eq!(f.focused, SshFormField::AuthMethod);
        // Agent: Secret visible (it is password-style for non-agent; actually Agent hides Secret).
        // With Agent selected, Secret is NOT visible. Next should be Alias.
        f.tab(); // Alias
        assert_eq!(f.focused, SshFormField::Alias);
        f.tab(); // Submit
        assert_eq!(f.focused, SshFormField::Submit);
        f.tab(); // wraps back to Host
        assert_eq!(f.focused, SshFormField::Host);
    }

    // ── Test 3 ───────────────────────────────────────────────────────────────

    #[test]
    fn back_tab_reverses_cycle() {
        let mut f = SshFormState::new();
        // Agent: visible = Host, Port, User, AuthMethod, Alias, Submit
        // back_tab from Host should go to Submit
        f.back_tab();
        assert_eq!(f.focused, SshFormField::Submit);
        f.back_tab();
        assert_eq!(f.focused, SshFormField::Alias);
    }

    // ── Test 4 ───────────────────────────────────────────────────────────────

    #[test]
    fn key_path_and_secret_visible_only_for_key_auth() {
        let mut f = SshFormState::new();
        f.auth_index = AUTH_IDX_KEY;
        let visible = f.visible_fields();
        assert!(visible.contains(&SshFormField::KeyPath));
        assert!(visible.contains(&SshFormField::Secret)); // passphrase

        f.auth_index = AUTH_IDX_AGENT;
        let visible = f.visible_fields();
        assert!(!visible.contains(&SshFormField::KeyPath));
        assert!(!visible.contains(&SshFormField::Secret));
    }

    // ── Test 5 ───────────────────────────────────────────────────────────────

    #[test]
    fn validate_rejects_empty_host() {
        let f = SshFormState::new();
        let err = f.validate().unwrap_err();
        assert!(err.to_lowercase().contains("host"), "unexpected: {err}");
    }

    // ── Test 6 ───────────────────────────────────────────────────────────────

    #[test]
    fn validate_rejects_bad_port() {
        let mut f = filled_form();
        f.port_str = "notanumber".into();
        assert!(f.validate().is_err());

        f.port_str = "0".into();
        assert!(f.validate().is_err());
    }

    // ── Test 7 ───────────────────────────────────────────────────────────────

    #[test]
    fn validate_ok_returns_ssh_config() {
        let f = filled_form();
        let cfg = f.validate().expect("should be valid");
        assert_eq!(cfg.host, "example.com");
        assert_eq!(cfg.port, 22);
        assert_eq!(cfg.user, "admin");
        assert!(matches!(cfg.auth, SshAuthMethod::Agent));
    }

    // ── Test 8 ───────────────────────────────────────────────────────────────

    #[test]
    fn esc_key_returns_cancelled() {
        let mut f = SshFormState::new();
        let result = f.handle_key(key(KeyCode::Esc));
        assert!(matches!(result, Some(SshFormResult::Cancelled)));
    }

    // ── Test 9 (bonus: prefill round-trip) ───────────────────────────────────

    #[test]
    fn prefill_round_trips_password_config() {
        let config = SshConfig {
            host: "srv.example.com".into(),
            port: 2222,
            user: "root".into(),
            auth: SshAuthMethod::Password("hunter2".into()),
        };
        let mut f = SshFormState::new();
        f.prefill(&config, Some("my-server"));

        assert_eq!(f.host, "srv.example.com");
        assert_eq!(f.port_str, "2222");
        assert_eq!(f.user, "root");
        assert_eq!(f.auth_index, AUTH_IDX_PASSWORD);
        assert_eq!(f.secret, "hunter2");
        assert_eq!(f.alias, "my-server");
    }
}
