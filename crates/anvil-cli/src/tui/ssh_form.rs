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
//! - Enter on [Browse]    open `~/.ssh` key picker overlay (KeyFile auth)
//! - Ctrl+F                same as activating [Browse], works from any field
//!                         when KeyFile auth is selected
//!
//! ## Key-path resolution
//! `resolve_key_path` lets the user type a bare filename (e.g. `id_ed25519`)
//! and have it auto-resolve against `~/.ssh/`. `~`-prefixed paths expand to
//! `$HOME`; anything containing a `/` is treated as an explicit path.

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

// ─── Key-path resolution ─────────────────────────────────────────────────────

/// Resolve a user-entered key path against `~/.ssh/`.
///
/// Rules:
/// - bare filename (no `/`, no leading `~`): join with `$HOME/.ssh/`
/// - `~/...` or `~`: expand `~` to `$HOME`
/// - anything containing `/`: returned as-is (treated as explicit path)
///
/// Returns the entered path unchanged if `$HOME` cannot be determined.
pub fn resolve_key_path(entered: &str) -> PathBuf {
    let trimmed = entered.trim();
    let home = dirs_next::home_dir();

    if let Some(stripped) = trimmed.strip_prefix("~/") {
        if let Some(h) = home {
            return h.join(stripped);
        }
    }
    if trimmed == "~" {
        if let Some(h) = home {
            return h;
        }
    }
    if trimmed.contains('/') {
        return PathBuf::from(trimmed);
    }
    // Bare filename — join under ~/.ssh/.
    if let Some(h) = home {
        return h.join(".ssh").join(trimmed);
    }
    PathBuf::from(trimmed)
}

/// Inverse of `resolve_key_path` for display: collapse a path under `~/.ssh/`
/// to its bare filename, and `$HOME/...` to `~/...`. Falls back to the full
/// string for any path outside the user's home.
pub fn collapse_key_path(path: &std::path::Path) -> String {
    let Some(home) = dirs_next::home_dir() else {
        return path.to_string_lossy().into_owned();
    };
    let ssh_dir = home.join(".ssh");
    if let Ok(stripped) = path.strip_prefix(&ssh_dir) {
        // Only collapse if it's a direct child (no further slashes).
        if stripped.components().count() == 1 {
            return stripped.to_string_lossy().into_owned();
        }
    }
    if let Ok(stripped) = path.strip_prefix(&home) {
        return format!("~/{}", stripped.to_string_lossy());
    }
    path.to_string_lossy().into_owned()
}

/// Decide whether a file name in `~/.ssh` should appear in the picker.
/// Hides public keys, the SSH config file, known_hosts, and dotfiles.
fn is_pickable_ssh_entry(name: &str) -> bool {
    if name.starts_with('.') {
        return false;
    }
    if name.ends_with(".pub") {
        return false;
    }
    if name == "config" {
        return false;
    }
    if name.starts_with("known_hosts") {
        return false;
    }
    if name == "authorized_keys" {
        return false;
    }
    true
}

/// Scan `~/.ssh/` and return private-key candidates, sorted.
pub fn scan_ssh_keys() -> Vec<String> {
    let Some(home) = dirs_next::home_dir() else { return Vec::new() };
    let dir = home.join(".ssh");
    let Ok(read) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out: Vec<String> = read
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| is_pickable_ssh_entry(name))
        .collect();
    out.sort();
    out
}

// ─── Key picker ──────────────────────────────────────────────────────────────

/// Sub-modal listing keys in `~/.ssh/`. Filter-as-you-type narrows the list.
#[derive(Debug, Clone)]
pub struct SshKeyPicker {
    /// All pickable entries (un-filtered, fixed for the lifetime of this picker).
    pub entries: Vec<String>,
    /// Current filter string typed by the user.
    pub filter: String,
    /// Index into the filtered view.
    pub selected: usize,
    /// Set when the directory was unreadable or empty.
    pub note: Option<String>,
}

impl SshKeyPicker {
    pub fn open() -> Self {
        let entries = scan_ssh_keys();
        let note = if entries.is_empty() {
            Some("No private-key candidates found in ~/.ssh/".to_string())
        } else {
            None
        };
        Self {
            entries,
            filter: String::new(),
            selected: 0,
            note,
        }
    }

    /// Filtered view of `entries` matching the current filter (case-insensitive substring).
    pub fn filtered(&self) -> Vec<&str> {
        if self.filter.is_empty() {
            return self.entries.iter().map(String::as_str).collect();
        }
        let needle = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|e| e.to_lowercase().contains(&needle))
            .map(String::as_str)
            .collect()
    }

    /// Clamp `selected` to the filtered list bounds. Call after filter changes.
    fn clamp_selected(&mut self) {
        let len = self.filtered().len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    /// Take the currently-selected filename, if any.
    pub fn take_selection(&self) -> Option<String> {
        self.filtered().get(self.selected).map(|s| (*s).to_string())
    }
}

// ─── Enums ───────────────────────────────────────────────────────────────────

/// Which field holds keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshFormField {
    Host,
    Port,
    User,
    AuthMethod,
    KeyPath,
    Browse,
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
        SshFormField::Browse,
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
    /// When `Some`, the key-picker sub-modal is open and consumes all input.
    pub picker: Option<SshKeyPicker>,
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
            picker: None,
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
                // If the saved key lives under ~/.ssh, show just the filename
                // so editing the form matches how the user typed it originally.
                self.key_path = collapse_key_path(path);
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
                SshFormField::KeyPath | SshFormField::Browse => self.key_path_visible(),
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
                let entered = self.key_path.trim().to_string();
                if entered.is_empty() {
                    return Err("Key file is required (bare filename uses ~/.ssh/)".into());
                }
                let resolved = resolve_key_path(&entered);
                if !resolved.exists() {
                    return Err(format!(
                        "Key file not found: {}",
                        resolved.display()
                    ));
                }
                let passphrase = if self.secret.is_empty() {
                    None
                } else {
                    Some(self.secret.clone())
                };
                SshAuthMethod::KeyFile {
                    path: resolved,
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
        // Picker sub-modal consumes all input while open.
        if self.picker.is_some() {
            self.handle_picker_key(key);
            return None;
        }

        // Ctrl+F shortcut: open the key picker from anywhere in the form
        // when KeyFile auth is selected. Mirrors the [Browse] button.
        if key.code == KeyCode::Char('f')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && self.key_path_visible()
        {
            self.picker = Some(SshKeyPicker::open());
            self.error = None;
            return None;
        }

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
                if self.focused == SshFormField::Browse {
                    // Activate the [Browse] button: open the key picker.
                    self.picker = Some(SshKeyPicker::open());
                    self.error = None;
                    return None;
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

    /// Dispatch a key into the open picker sub-modal.
    fn handle_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.picker.as_mut() else { return };
        match key.code {
            KeyCode::Esc => {
                self.picker = None;
            }
            KeyCode::Enter => {
                if let Some(name) = picker.take_selection() {
                    self.key_path = name;
                    self.error = None;
                }
                self.picker = None;
            }
            KeyCode::Up => {
                if picker.selected > 0 {
                    picker.selected -= 1;
                }
            }
            KeyCode::Down => {
                let len = picker.filtered().len();
                if len > 0 && picker.selected + 1 < len {
                    picker.selected += 1;
                }
            }
            KeyCode::Char(c) => {
                picker.filter.push(c);
                picker.clamp_selected();
            }
            KeyCode::Backspace => {
                picker.filter.pop();
                picker.clamp_selected();
            }
            _ => {}
        }
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
            SshFormField::AuthMethod | SshFormField::Browse | SshFormField::Submit => None,
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
        let modal_h: u16 = 23;
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

        // Key path (only when KeyFile selected) + [Browse] button
        if self.key_path_visible() {
            lines.push(make_label(
                "  Key (bare name resolves under ~/.ssh/):",
                focused == SshFormField::KeyPath,
            ));
            lines.push(make_input(&self.key_path, focused == SshFormField::KeyPath, false));
            let browse_active = focused == SshFormField::Browse;
            let browse_style = if browse_active {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Yellow)
            };
            lines.push(Line::from(Span::styled(
                "  [ Browse ~/.ssh ]  ",
                browse_style,
            )));
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

        // Hint line (mentions Ctrl+F only when KeyFile auth is selected)
        let hint = if self.key_path_visible() {
            "  Tab/Enter=next  Shift+Tab=prev  Esc=cancel  Ctrl+F=browse keys"
        } else {
            "  Tab/Enter=next  Shift+Tab=prev  Esc=cancel"
        };
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        )));

        let para = Paragraph::new(lines);
        frame.render_widget(para, inner);

        // Picker overlay on top of the form.
        if let Some(ref picker) = self.picker {
            render_picker(frame, area, picker);
        }
    }
}

/// Render the key-picker sub-modal centered within `outer`.
fn render_picker(frame: &mut ratatui::Frame, outer: Rect, picker: &SshKeyPicker) {
    let w: u16 = 50;
    let h: u16 = 14;
    let x = outer.x + (outer.width.saturating_sub(w)) / 2;
    let y = outer.y + (outer.height.saturating_sub(h)) / 2;
    let area = Rect {
        x,
        y,
        width: w.min(outer.width),
        height: h.min(outer.height),
    };

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Browse ~/.ssh ")
        .title_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("  Filter: {}_", picker.filter),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));

    let entries = picker.filtered();
    if let Some(ref note) = picker.note {
        lines.push(Line::from(Span::styled(
            format!("  {note}"),
            Style::default().fg(Color::DarkGray),
        )));
    } else if entries.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no matches)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        // Show at most 8 rows around the selection.
        let max_rows = 8usize;
        let start = picker.selected.saturating_sub(max_rows / 2);
        for (i, name) in entries.iter().enumerate().skip(start).take(max_rows) {
            let is_sel = i == picker.selected;
            let prefix = if is_sel { "> " } else { "  " };
            let style = if is_sel {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(Span::styled(
                format!(" {prefix}{name} "),
                style,
            )));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  Enter=select  Esc=back  Up/Down=nav  type to filter",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(Paragraph::new(lines), inner);
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

    // ── Resolver tests ───────────────────────────────────────────────────────

    #[test]
    fn resolver_bare_filename_joins_under_dot_ssh() {
        let resolved = resolve_key_path("id_ed25519");
        let home = dirs_next::home_dir().expect("home should be available in tests");
        assert_eq!(resolved, home.join(".ssh").join("id_ed25519"));
    }

    #[test]
    fn resolver_tilde_path_expands_home() {
        let resolved = resolve_key_path("~/keys/work_key");
        let home = dirs_next::home_dir().expect("home should be available in tests");
        assert_eq!(resolved, home.join("keys").join("work_key"));
    }

    #[test]
    fn resolver_absolute_path_left_alone() {
        let resolved = resolve_key_path("/tmp/extra/key");
        assert_eq!(resolved, PathBuf::from("/tmp/extra/key"));
    }

    #[test]
    fn resolver_relative_with_slash_is_not_rewritten() {
        // Paths containing `/` (other than tilde-prefixed) are taken literally.
        let resolved = resolve_key_path("subdir/key");
        assert_eq!(resolved, PathBuf::from("subdir/key"));
    }

    #[test]
    fn collapse_under_dot_ssh_shows_bare_name() {
        let home = dirs_next::home_dir().expect("home should be available in tests");
        let full = home.join(".ssh").join("id_ed25519");
        assert_eq!(collapse_key_path(&full), "id_ed25519");
    }

    #[test]
    fn collapse_outside_home_keeps_full_path() {
        assert_eq!(
            collapse_key_path(&PathBuf::from("/etc/ssh/private")),
            "/etc/ssh/private"
        );
    }

    #[test]
    fn validate_rejects_missing_key_file() {
        let mut f = filled_form();
        f.auth_index = AUTH_IDX_KEY;
        f.key_path = "this-key-does-not-exist-anywhere-xyz".into();
        let err = f.validate().unwrap_err();
        assert!(
            err.contains("not found"),
            "expected 'not found' error, got: {err}"
        );
    }

    // ── Picker tests ─────────────────────────────────────────────────────────

    fn picker_with(entries: &[&str]) -> SshKeyPicker {
        SshKeyPicker {
            entries: entries.iter().map(|s| (*s).to_string()).collect(),
            filter: String::new(),
            selected: 0,
            note: None,
        }
    }

    #[test]
    fn is_pickable_entry_filters_out_pub_config_known_hosts_dotfiles() {
        assert!(is_pickable_ssh_entry("id_ed25519"));
        assert!(is_pickable_ssh_entry("culpur_key"));
        assert!(!is_pickable_ssh_entry("id_ed25519.pub"));
        assert!(!is_pickable_ssh_entry("config"));
        assert!(!is_pickable_ssh_entry("known_hosts"));
        assert!(!is_pickable_ssh_entry("known_hosts.old"));
        assert!(!is_pickable_ssh_entry(".anvil"));
        assert!(!is_pickable_ssh_entry("authorized_keys"));
    }

    #[test]
    fn picker_filter_narrows_case_insensitively() {
        let mut p = picker_with(&["id_ed25519", "Work_Key", "culpur_key", "other"]);
        p.filter = "KEY".into();
        let view = p.filtered();
        assert_eq!(view, vec!["Work_Key", "culpur_key"]);
    }

    #[test]
    fn picker_enter_fills_key_path_and_closes_picker() {
        let mut f = SshFormState::new();
        f.auth_index = AUTH_IDX_KEY;
        f.focused = SshFormField::KeyPath;
        f.picker = Some(picker_with(&["id_ed25519", "work_key"]));
        if let Some(p) = f.picker.as_mut() {
            p.selected = 1;
        }
        let _ = f.handle_key(key(KeyCode::Enter));
        assert!(f.picker.is_none());
        assert_eq!(f.key_path, "work_key");
    }

    #[test]
    fn picker_esc_returns_to_form_without_changing_key_path() {
        let mut f = SshFormState::new();
        f.auth_index = AUTH_IDX_KEY;
        f.focused = SshFormField::KeyPath;
        f.key_path = "existing".into();
        f.picker = Some(picker_with(&["one", "two"]));
        let _ = f.handle_key(key(KeyCode::Esc));
        assert!(f.picker.is_none());
        assert_eq!(f.key_path, "existing");
    }

    #[test]
    fn ctrl_f_opens_picker_when_key_auth_is_selected() {
        let mut f = SshFormState::new();
        f.auth_index = AUTH_IDX_KEY;
        // Any focus works as long as KeyFile auth is selected.
        f.focused = SshFormField::Host;
        let _ = f.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(f.picker.is_some());
    }

    #[test]
    fn ctrl_f_does_nothing_under_agent_auth() {
        let mut f = SshFormState::new();
        f.auth_index = AUTH_IDX_AGENT;
        let _ = f.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(f.picker.is_none());
    }

    #[test]
    fn enter_on_browse_button_opens_picker() {
        let mut f = SshFormState::new();
        f.auth_index = AUTH_IDX_KEY;
        f.focused = SshFormField::Browse;
        let _ = f.handle_key(key(KeyCode::Enter));
        assert!(f.picker.is_some());
    }

    #[test]
    fn browse_field_visible_only_for_key_auth() {
        let mut f = SshFormState::new();
        f.auth_index = AUTH_IDX_KEY;
        assert!(f.visible_fields().contains(&SshFormField::Browse));
        f.auth_index = AUTH_IDX_AGENT;
        assert!(!f.visible_fields().contains(&SshFormField::Browse));
    }

    // ── Existing test resumes ────────────────────────────────────────────────

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
