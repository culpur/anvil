/// Theme system for Anvil TUI.
///
/// A `Theme` holds a palette of RGB triples for every named slot used by the
/// TUI.  The current theme is persisted to `~/.anvil/theme.json` so it
/// survives across sessions.  Five built-in themes are provided; users can
/// also hand-edit the JSON to create custom palettes.
///
/// The `StatusLineConfig` system provides widget-based customization of the
/// TUI status bar.  Users choose a preset or configure individual widgets.
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// Re-export Color so callers do not need to depend on ratatui directly.
// We use a local newtype instead to avoid adding ratatui to the runtime crate's
// dependency list — the TUI helper methods live in tui.rs (anvil-cli) and
// call .r / .g / .b directly.

/// A single RGB colour triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parse a `#rrggbb` hex string.
    fn from_hex(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        Some(Self(r, g, b))
    }

    fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

// ─── JSON wire format ─────────────────────────────────────────────────────────

/// Serialised representation stored in `~/.anvil/theme.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ThemeFile {
    name: String,
    colors: ThemeColors,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ThemeColors {
    bg_primary: String,
    bg_card: String,
    text_primary: String,
    text_secondary: String,
    accent: String,
    accent_secondary: String,
    success: String,
    warning: String,
    error: String,
    border: String,
    header_bg: String,
    thinking: String,
}

// ─── Theme ────────────────────────────────────────────────────────────────────

/// The full palette used by the TUI.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub bg_primary: Rgb,
    pub bg_card: Rgb,
    pub text_primary: Rgb,
    pub text_secondary: Rgb,
    pub accent: Rgb,
    pub accent_secondary: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub error: Rgb,
    pub border: Rgb,
    pub header_bg: Rgb,
    pub thinking: Rgb,
}

impl Theme {
    // ── Persistence ──────────────────────────────────────────────────────────

    fn theme_path() -> Option<PathBuf> {
        dirs_home().map(|h| h.join(".anvil").join("theme.json"))
    }

    /// Load the active theme from `~/.anvil/theme.json`.
    /// Falls back to `default_theme()` if the file is missing or invalid.
    #[must_use]
    pub fn load() -> Self {
        Self::try_load().unwrap_or_else(Self::default_theme)
    }

    fn try_load() -> Option<Self> {
        let path = Self::theme_path()?;
        let text = std::fs::read_to_string(path).ok()?;
        let file: ThemeFile = serde_json::from_str(&text).ok()?;
        Self::from_file(file)
    }

    /// Persist this theme to `~/.anvil/theme.json`.
    pub fn save(&self) -> io::Result<()> {
        let path = Self::theme_path().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "cannot determine home directory")
        })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = ThemeFile {
            name: self.name.clone(),
            colors: ThemeColors {
                bg_primary: self.bg_primary.to_hex(),
                bg_card: self.bg_card.to_hex(),
                text_primary: self.text_primary.to_hex(),
                text_secondary: self.text_secondary.to_hex(),
                accent: self.accent.to_hex(),
                accent_secondary: self.accent_secondary.to_hex(),
                success: self.success.to_hex(),
                warning: self.warning.to_hex(),
                error: self.error.to_hex(),
                border: self.border.to_hex(),
                header_bg: self.header_bg.to_hex(),
                thinking: self.thinking.to_hex(),
            },
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }

    // ── Built-in themes ──────────────────────────────────────────────────────

    /// Return the culpur-defense default theme.
    #[must_use]
    pub fn default_theme() -> Self {
        Self::culpur_defense()
    }

    /// Return a built-in theme by name, or `None` if unknown.
    #[must_use]
    pub fn builtin(name: &str) -> Option<Self> {
        match name {
            "culpur-defense" | "default" => Some(Self::culpur_defense()),
            "cyberpunk" => Some(Self::cyberpunk()),
            "nord" => Some(Self::nord()),
            "solarized-dark" => Some(Self::solarized_dark()),
            "dracula" => Some(Self::dracula()),
            "monokai" => Some(Self::monokai()),
            "gruvbox" => Some(Self::gruvbox()),
            "catppuccin" => Some(Self::catppuccin()),
            _ => None,
        }
    }

    /// Names of all built-in themes.
    #[must_use]
    pub fn builtin_names() -> &'static [&'static str] {
        &[
            "culpur-defense",
            "cyberpunk",
            "nord",
            "solarized-dark",
            "dracula",
            "monokai",
            "gruvbox",
            "catppuccin",
        ]
    }

    fn culpur_defense() -> Self {
        Self {
            name: "culpur-defense".to_string(),
            bg_primary:       Rgb(0x00, 0x02, 0x0C),
            bg_card:          Rgb(0x0F, 0x17, 0x2A),
            text_primary:     Rgb(0xFF, 0xFF, 0xFF),
            text_secondary:   Rgb(0x94, 0xA3, 0xB8),
            accent:           Rgb(0x0F, 0xBC, 0xFF),
            accent_secondary: Rgb(0x00, 0x85, 0xFF),
            success:          Rgb(0x00, 0xD0, 0x84),
            warning:          Rgb(0xF5, 0x9E, 0x0B),
            error:            Rgb(0xEF, 0x44, 0x44),
            border:           Rgb(0x1E, 0x29, 0x3B),
            header_bg:        Rgb(0x1A, 0x1A, 0x2E),
            thinking:         Rgb(0x0F, 0xBC, 0xFF),
        }
    }

    fn cyberpunk() -> Self {
        Self {
            name: "cyberpunk".to_string(),
            bg_primary:       Rgb(0x0A, 0x0A, 0x1A),
            bg_card:          Rgb(0x1A, 0x1A, 0x2E),
            text_primary:     Rgb(0xE0, 0xE0, 0xFF),
            text_secondary:   Rgb(0x88, 0x88, 0xAA),
            accent:           Rgb(0xFF, 0x2D, 0x95),
            accent_secondary: Rgb(0x00, 0xD4, 0xFF),
            success:          Rgb(0x39, 0xFF, 0x14),
            warning:          Rgb(0xF1, 0xFA, 0x8C),
            error:            Rgb(0xFF, 0x33, 0x33),
            border:           Rgb(0x2A, 0x1A, 0x3E),
            header_bg:        Rgb(0x12, 0x00, 0x1E),
            thinking:         Rgb(0xFF, 0x2D, 0x95),
        }
    }

    fn nord() -> Self {
        Self {
            name: "nord".to_string(),
            bg_primary:       Rgb(0x2E, 0x34, 0x40),
            bg_card:          Rgb(0x3B, 0x42, 0x52),
            text_primary:     Rgb(0xEC, 0xEF, 0xF4),
            text_secondary:   Rgb(0xD8, 0xDE, 0xE9),
            accent:           Rgb(0x88, 0xC0, 0xD0),
            accent_secondary: Rgb(0x81, 0xA1, 0xC1),
            success:          Rgb(0xA3, 0xBE, 0x8C),
            warning:          Rgb(0xEB, 0xCB, 0x8B),
            error:            Rgb(0xBF, 0x61, 0x6A),
            border:           Rgb(0x43, 0x4C, 0x5E),
            header_bg:        Rgb(0x2E, 0x34, 0x40),
            thinking:         Rgb(0x88, 0xC0, 0xD0),
        }
    }

    fn solarized_dark() -> Self {
        Self {
            name: "solarized-dark".to_string(),
            bg_primary:       Rgb(0x00, 0x2B, 0x36),
            bg_card:          Rgb(0x07, 0x36, 0x42),
            text_primary:     Rgb(0x83, 0x94, 0x96),
            text_secondary:   Rgb(0x65, 0x7B, 0x83),
            accent:           Rgb(0x26, 0x8B, 0xD2),
            accent_secondary: Rgb(0x2A, 0xA1, 0x98),
            success:          Rgb(0x85, 0x99, 0x00),
            warning:          Rgb(0xB5, 0x89, 0x00),
            error:            Rgb(0xDC, 0x32, 0x2F),
            border:           Rgb(0x07, 0x36, 0x42),
            header_bg:        Rgb(0x00, 0x2B, 0x36),
            thinking:         Rgb(0x26, 0x8B, 0xD2),
        }
    }

    fn dracula() -> Self {
        Self {
            name: "dracula".to_string(),
            bg_primary:       Rgb(0x28, 0x2A, 0x36),
            bg_card:          Rgb(0x44, 0x47, 0x5A),
            text_primary:     Rgb(0xF8, 0xF8, 0xF2),
            text_secondary:   Rgb(0x62, 0x72, 0xA4),
            accent:           Rgb(0xBD, 0x93, 0xF9),
            accent_secondary: Rgb(0xFF, 0x79, 0xC6),
            success:          Rgb(0x50, 0xFA, 0x7B),
            warning:          Rgb(0xF1, 0xFA, 0x8C),
            error:            Rgb(0xFF, 0x55, 0x55),
            border:           Rgb(0x44, 0x47, 0x5A),
            header_bg:        Rgb(0x28, 0x2A, 0x36),
            thinking:         Rgb(0xBD, 0x93, 0xF9),
        }
    }

    fn monokai() -> Self {
        Self {
            name: "monokai".to_string(),
            bg_primary:       Rgb(0x27, 0x28, 0x22),
            bg_card:          Rgb(0x3E, 0x3D, 0x32),
            text_primary:     Rgb(0xF8, 0xF8, 0xF2),
            text_secondary:   Rgb(0x75, 0x71, 0x5E),
            accent:           Rgb(0xA6, 0xE2, 0x2E),
            accent_secondary: Rgb(0x66, 0xD9, 0xE8),
            success:          Rgb(0xA6, 0xE2, 0x2E),
            warning:          Rgb(0xE6, 0xDB, 0x74),
            error:            Rgb(0xF9, 0x26, 0x72),
            border:           Rgb(0x49, 0x48, 0x3E),
            header_bg:        Rgb(0x1E, 0x1F, 0x1C),
            thinking:         Rgb(0xAE, 0x81, 0xFF),
        }
    }

    fn gruvbox() -> Self {
        Self {
            name: "gruvbox".to_string(),
            bg_primary:       Rgb(0x28, 0x28, 0x28),
            bg_card:          Rgb(0x3C, 0x38, 0x36),
            text_primary:     Rgb(0xEB, 0xDB, 0xB2),
            text_secondary:   Rgb(0xA8, 0x99, 0x84),
            accent:           Rgb(0xD7, 0x99, 0x21),
            accent_secondary: Rgb(0xB8, 0xBB, 0x26),
            success:          Rgb(0xB8, 0xBB, 0x26),
            warning:          Rgb(0xFE, 0x80, 0x19),
            error:            Rgb(0xCC, 0x24, 0x1D),
            border:           Rgb(0x50, 0x49, 0x45),
            header_bg:        Rgb(0x1D, 0x20, 0x21),
            thinking:         Rgb(0x83, 0xA5, 0x98),
        }
    }

    fn catppuccin() -> Self {
        Self {
            name: "catppuccin".to_string(),
            bg_primary:       Rgb(0x1E, 0x1E, 0x2E),
            bg_card:          Rgb(0x31, 0x32, 0x44),
            text_primary:     Rgb(0xCA, 0xD3, 0xF5),
            text_secondary:   Rgb(0xA5, 0xAD, 0xCE),
            accent:           Rgb(0xCA, 0xA6, 0xF7),
            accent_secondary: Rgb(0xF5, 0xBD, 0xE2),
            success:          Rgb(0xA6, 0xDA, 0x95),
            warning:          Rgb(0xEE, 0xD4, 0x9F),
            error:            Rgb(0xED, 0x87, 0x96),
            border:           Rgb(0x45, 0x47, 0x5A),
            header_bg:        Rgb(0x18, 0x18, 0x26),
            thinking:         Rgb(0x8B, 0xD5, 0xCA),
        }
    }

    // ── JSON round-trip ──────────────────────────────────────────────────────

    fn from_file(f: ThemeFile) -> Option<Self> {
        Some(Self {
            name: f.name,
            bg_primary:       Rgb::from_hex(&f.colors.bg_primary)?,
            bg_card:          Rgb::from_hex(&f.colors.bg_card)?,
            text_primary:     Rgb::from_hex(&f.colors.text_primary)?,
            text_secondary:   Rgb::from_hex(&f.colors.text_secondary)?,
            accent:           Rgb::from_hex(&f.colors.accent)?,
            accent_secondary: Rgb::from_hex(&f.colors.accent_secondary)?,
            success:          Rgb::from_hex(&f.colors.success)?,
            warning:          Rgb::from_hex(&f.colors.warning)?,
            error:            Rgb::from_hex(&f.colors.error)?,
            border:           Rgb::from_hex(&f.colors.border)?,
            header_bg:        Rgb::from_hex(&f.colors.header_bg)?,
            thinking:         Rgb::from_hex(&f.colors.thinking)?,
        })
    }
}

// ── Minimal home-dir helper (avoids a new dependency) ────────────────────────

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

// ─── Status Line Widget System ───────────────────────────────────────────────

/// A single widget that can appear in the status line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusWidget {
    // Model & AI
    Model,
    Thinking,
    Effort,
    Provider,
    // Tokens & Cost
    TokensTotal,
    TokensInput,
    TokensOutput,
    Cost,
    TokenSpeed,
    // Context
    ContextBar,
    ContextPct,
    ContextTokens,
    // Session
    SessionTime,
    SessionPct,
    BlockTime,
    // Git
    GitBranch,
    GitStatus,
    GitDiff,
    // System
    Permissions,
    QmdStatus,
    Version,
    VimMode,
    RemoteControl,
    UpdateAvailable,
    ArchiveStatus,
    McpStatus,
    TimeDisplay,
    // Cost breakdown
    BurnRate,
    CostDaily,
    CostWeekly,
    CostMonthly,
    CostProjection,
    CacheHitRate,
    // Productivity
    CodeProductivity,
    // Custom
    Text { content: String },
    Spacer,
    Separator,
}

impl StatusWidget {
    /// Short identifier for this widget type.
    #[must_use] 
    pub fn id(&self) -> &str {
        match self {
            Self::Model => "model",
            Self::Thinking => "thinking",
            Self::Effort => "effort",
            Self::Provider => "provider",
            Self::TokensTotal => "tokens_total",
            Self::TokensInput => "tokens_input",
            Self::TokensOutput => "tokens_output",
            Self::Cost => "cost",
            Self::TokenSpeed => "token_speed",
            Self::ContextBar => "context_bar",
            Self::ContextPct => "context_pct",
            Self::ContextTokens => "context_tokens",
            Self::SessionTime => "session_time",
            Self::SessionPct => "session_pct",
            Self::BlockTime => "block_time",
            Self::GitBranch => "git_branch",
            Self::GitStatus => "git_status",
            Self::GitDiff => "git_diff",
            Self::Permissions => "permissions",
            Self::QmdStatus => "qmd_status",
            Self::Version => "version",
            Self::VimMode => "vim_mode",
            Self::RemoteControl => "remote_control",
            Self::UpdateAvailable => "update_available",
            Self::ArchiveStatus => "archive_status",
            Self::McpStatus => "mcp_status",
            Self::TimeDisplay => "time_display",
            Self::BurnRate => "burn_rate",
            Self::CostDaily => "cost_daily",
            Self::CostWeekly => "cost_weekly",
            Self::CostMonthly => "cost_monthly",
            Self::CostProjection => "cost_projection",
            Self::CacheHitRate => "cache_hit_rate",
            Self::CodeProductivity => "code_productivity",
            Self::Text { .. } => "text",
            Self::Spacer => "spacer",
            Self::Separator => "separator",
        }
    }

    /// Human-readable label for the config panel.
    #[must_use] 
    pub fn display_name(&self) -> &str {
        match self {
            Self::Model => "Model",
            Self::Thinking => "Thinking",
            Self::Effort => "Effort",
            Self::Provider => "Provider",
            Self::TokensTotal => "Total Tokens",
            Self::TokensInput => "Input Tokens",
            Self::TokensOutput => "Output Tokens",
            Self::Cost => "Cost",
            Self::TokenSpeed => "Token Speed",
            Self::ContextBar => "Context Bar",
            Self::ContextPct => "Context %",
            Self::ContextTokens => "Context Tokens",
            Self::SessionTime => "Session Time",
            Self::SessionPct => "Session %",
            Self::BlockTime => "Block Time",
            Self::GitBranch => "Git Branch",
            Self::GitStatus => "Git Status",
            Self::GitDiff => "Git Diff",
            Self::Permissions => "Permissions",
            Self::QmdStatus => "QMD Status",
            Self::Version => "Version",
            Self::VimMode => "Vim Mode",
            Self::RemoteControl => "Remote Control",
            Self::UpdateAvailable => "Update Available",
            Self::ArchiveStatus => "Archive Status",
            Self::McpStatus => "MCP Status",
            Self::TimeDisplay => "Time",
            Self::BurnRate => "Burn Rate",
            Self::CostDaily => "Cost (Daily)",
            Self::CostWeekly => "Cost (Weekly)",
            Self::CostMonthly => "Cost (Monthly)",
            Self::CostProjection => "Cost Projection",
            Self::CacheHitRate => "Cache Hit Rate",
            Self::CodeProductivity => "Code Productivity",
            Self::Text { .. } => "Text",
            Self::Spacer => "Spacer",
            Self::Separator => "Separator",
        }
    }

    /// Widget category for color-coding in the editor UI.
    #[must_use] 
    pub fn category(&self) -> &str {
        match self {
            Self::Model | Self::Thinking | Self::Effort | Self::Provider => "model",
            Self::TokensTotal | Self::TokensInput | Self::TokensOutput
            | Self::Cost | Self::TokenSpeed => "tokens",
            Self::ContextBar | Self::ContextPct | Self::ContextTokens => "context",
            Self::SessionTime | Self::SessionPct | Self::BlockTime => "session",
            Self::GitBranch | Self::GitStatus | Self::GitDiff => "git",
            Self::Permissions | Self::QmdStatus | Self::Version | Self::VimMode
            | Self::RemoteControl | Self::UpdateAvailable | Self::ArchiveStatus
            | Self::McpStatus | Self::TimeDisplay => "system",
            Self::BurnRate | Self::CostDaily | Self::CostWeekly | Self::CostMonthly
            | Self::CostProjection | Self::CacheHitRate => "cost_detail",
            Self::CodeProductivity => "productivity",
            Self::Text { .. } | Self::Spacer | Self::Separator => "layout",
        }
    }

    /// All concrete widget variants (excludes `Text { content }` which needs special handling).
    #[must_use] 
    pub fn all_widgets() -> Vec<StatusWidget> {
        vec![
            Self::Model, Self::Thinking, Self::Effort, Self::Provider,
            Self::TokensTotal, Self::TokensInput, Self::TokensOutput, Self::Cost, Self::TokenSpeed,
            Self::ContextBar, Self::ContextPct, Self::ContextTokens,
            Self::SessionTime, Self::SessionPct, Self::BlockTime,
            Self::GitBranch, Self::GitStatus, Self::GitDiff,
            Self::Permissions, Self::QmdStatus, Self::Version, Self::VimMode,
            Self::RemoteControl, Self::UpdateAvailable, Self::ArchiveStatus,
            Self::McpStatus, Self::TimeDisplay,
            Self::BurnRate, Self::CostDaily, Self::CostWeekly, Self::CostMonthly,
            Self::CostProjection, Self::CacheHitRate,
            Self::CodeProductivity,
            Self::Spacer, Self::Separator,
        ]
    }

    /// Parse a widget from its ID string (inverse of `id()`).
    #[must_use] 
    pub fn from_id(id: &str) -> Option<StatusWidget> {
        match id {
            "model" => Some(Self::Model),
            "thinking" => Some(Self::Thinking),
            "effort" => Some(Self::Effort),
            "provider" => Some(Self::Provider),
            "tokens_total" => Some(Self::TokensTotal),
            "tokens_input" => Some(Self::TokensInput),
            "tokens_output" => Some(Self::TokensOutput),
            "cost" => Some(Self::Cost),
            "token_speed" => Some(Self::TokenSpeed),
            "context_bar" => Some(Self::ContextBar),
            "context_pct" => Some(Self::ContextPct),
            "context_tokens" => Some(Self::ContextTokens),
            "session_time" => Some(Self::SessionTime),
            "session_pct" => Some(Self::SessionPct),
            "block_time" => Some(Self::BlockTime),
            "git_branch" => Some(Self::GitBranch),
            "git_status" => Some(Self::GitStatus),
            "git_diff" => Some(Self::GitDiff),
            "permissions" => Some(Self::Permissions),
            "qmd_status" => Some(Self::QmdStatus),
            "version" => Some(Self::Version),
            "vim_mode" => Some(Self::VimMode),
            "remote_control" => Some(Self::RemoteControl),
            "update_available" => Some(Self::UpdateAvailable),
            "archive_status" => Some(Self::ArchiveStatus),
            "mcp_status" => Some(Self::McpStatus),
            "time_display" => Some(Self::TimeDisplay),
            "burn_rate" => Some(Self::BurnRate),
            "cost_daily" => Some(Self::CostDaily),
            "cost_weekly" => Some(Self::CostWeekly),
            "cost_monthly" => Some(Self::CostMonthly),
            "cost_projection" => Some(Self::CostProjection),
            "cache_hit_rate" => Some(Self::CacheHitRate),
            "code_productivity" => Some(Self::CodeProductivity),
            "spacer" => Some(Self::Spacer),
            "separator" => Some(Self::Separator),
            _ => None,
        }
    }
}

/// Which side of a status line (left-aligned or right-aligned).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Left,
    Right,
}

/// One horizontal line in the status bar, with left-aligned and right-aligned widgets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusLine {
    pub left: Vec<StatusWidget>,
    pub right: Vec<StatusWidget>,
}

/// Per-widget style overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetStyle {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
}

/// Full status line configuration: lines + style settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusLineConfig {
    pub preset: String,
    pub lines: Vec<StatusLine>,
    #[serde(default = "default_separator")]
    pub separator_char: String,
    #[serde(default)]
    pub compact: bool,
    #[serde(default)]
    pub widgets: std::collections::HashMap<String, WidgetStyle>,
}

fn default_separator() -> String {
    " │ ".to_string()
}

/// Named presets for different user demographics and workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusLinePreset {
    // ── Core presets ──
    Default,
    Minimal,
    Developer,
    TokenHeavy,
    GitHeavy,
    Compact,
    CostFocused,
    Streamer,
    // ── Emoji-rich presets ──
    Gaming,
    Devops,
    BudgetTracker,
    Zen,
    Academic,
    Hacker,
    NightOwl,
    Dashboard,
}

impl StatusLinePreset {
    #[must_use] 
    pub fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Minimal => "minimal",
            Self::Developer => "developer",
            Self::TokenHeavy => "token-heavy",
            Self::GitHeavy => "git-heavy",
            Self::Compact => "compact",
            Self::CostFocused => "cost-focused",
            Self::Streamer => "streamer",
            Self::Gaming => "gaming",
            Self::Devops => "devops",
            Self::BudgetTracker => "budget-tracker",
            Self::Zen => "zen",
            Self::Academic => "academic",
            Self::Hacker => "hacker",
            Self::NightOwl => "night-owl",
            Self::Dashboard => "dashboard",
        }
    }

    #[must_use] 
    pub fn description(self) -> &'static str {
        match self {
            Self::Default => "Balanced layout — model, tokens, context, git, permissions",
            Self::Minimal => "Clean and quiet — model + context bar only",
            Self::Developer => "Full git info, permissions, QMD — for power users",
            Self::TokenHeavy => "Token counts front and center — for cost-conscious users",
            Self::GitHeavy => "Git branch, status, diff prominent — for commit-heavy workflows",
            Self::Compact => "Everything on 2 lines — maximizes content area",
            Self::CostFocused => "Cost and token speed prominent — for API budget tracking",
            Self::Streamer => "Large model name, no cost — clean for screen sharing",
            Self::Gaming => "\u{1f3ae} Emoji-rich gaming/streamer vibe with burn rate",
            Self::Devops => "\u{1f433} DevOps/SRE — MCP, permissions, git, RC prominent",
            Self::BudgetTracker => "\u{1f4b8} Full cost breakdown — daily/weekly/monthly + burn rate",
            Self::Zen => "\u{1f9d8} Ultra-minimal zen — just model + context, emoji accents",
            Self::Academic => "\u{1f4da} Learning — productivity, session time, token detail",
            Self::Hacker => "\u{1f480} Cyberpunk hacker — permissions, RC, MCP, full access",
            Self::NightOwl => "\u{1f319} Chill night-owl — time, session, relaxed layout",
            Self::Dashboard => "\u{1f4ca} Maximalist — every widget, 4 lines, full dashboard",
        }
    }

    #[must_use] 
    pub fn all() -> &'static [StatusLinePreset] {
        &[
            Self::Default,
            Self::Minimal,
            Self::Developer,
            Self::TokenHeavy,
            Self::GitHeavy,
            Self::Compact,
            Self::CostFocused,
            Self::Streamer,
            Self::Gaming,
            Self::Devops,
            Self::BudgetTracker,
            Self::Zen,
            Self::Academic,
            Self::Hacker,
            Self::NightOwl,
            Self::Dashboard,
        ]
    }

    #[must_use] 
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "default" => Some(Self::Default),
            "minimal" => Some(Self::Minimal),
            "developer" => Some(Self::Developer),
            "token-heavy" => Some(Self::TokenHeavy),
            "git-heavy" => Some(Self::GitHeavy),
            "compact" => Some(Self::Compact),
            "cost-focused" => Some(Self::CostFocused),
            "streamer" => Some(Self::Streamer),
            "gaming" => Some(Self::Gaming),
            "devops" => Some(Self::Devops),
            "budget-tracker" => Some(Self::BudgetTracker),
            "zen" => Some(Self::Zen),
            "academic" => Some(Self::Academic),
            "hacker" => Some(Self::Hacker),
            "night-owl" => Some(Self::NightOwl),
            "dashboard" => Some(Self::Dashboard),
            _ => None,
        }
    }
}

impl StatusLineConfig {
    /// Load from `~/.anvil/config.json` under the `"status_line"` key.
    /// Falls back to `default()` if missing or invalid.
    #[must_use] 
    pub fn load() -> Self {
        Self::try_load().unwrap_or_default()
    }

    fn try_load() -> Option<Self> {
        let home = dirs_home()?;
        let path = home.join(".anvil").join("config.json");
        let text = std::fs::read_to_string(path).ok()?;
        let obj: serde_json::Value = serde_json::from_str(&text).ok()?;
        let sl = obj.get("status_line")?;
        serde_json::from_value(sl.clone()).ok()
    }

    /// Serialize this config as a JSON value for embedding in config.json.
    #[must_use] 
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Number of status lines this config renders.
    #[must_use] 
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    // ── Mutation methods for the editor ─────────────────────────────

    /// Append a new empty status line.
    pub fn add_line(&mut self) {
        self.lines.push(StatusLine { left: vec![], right: vec![] });
        self.preset = "custom".into();
    }

    /// Remove a status line (no-op if only 1 line remains).
    pub fn remove_line(&mut self, idx: usize) {
        if self.lines.len() > 1 && idx < self.lines.len() {
            self.lines.remove(idx);
            self.preset = "custom".into();
        }
    }

    /// Add a widget to a line's left or right side.
    pub fn add_widget(&mut self, line_idx: usize, side: Side, widget: StatusWidget) {
        if let Some(line) = self.lines.get_mut(line_idx) {
            match side {
                Side::Left => line.left.push(widget),
                Side::Right => line.right.push(widget),
            }
            self.preset = "custom".into();
        }
    }

    /// Remove a widget from a line's left or right side.
    pub fn remove_widget(&mut self, line_idx: usize, side: Side, widget_idx: usize) {
        if let Some(line) = self.lines.get_mut(line_idx) {
            let vec = match side { Side::Left => &mut line.left, Side::Right => &mut line.right };
            if widget_idx < vec.len() {
                vec.remove(widget_idx);
                self.preset = "custom".into();
            }
        }
    }

    /// Move a widget within its side by `delta` positions (-1 = left, +1 = right).
    pub fn move_widget(&mut self, line_idx: usize, side: Side, widget_idx: usize, delta: i32) {
        if let Some(line) = self.lines.get_mut(line_idx) {
            let vec = match side { Side::Left => &mut line.left, Side::Right => &mut line.right };
            let new_idx = (widget_idx as i32 + delta).clamp(0, vec.len().saturating_sub(1) as i32) as usize;
            if new_idx != widget_idx && widget_idx < vec.len() {
                vec.swap(widget_idx, new_idx);
                self.preset = "custom".into();
            }
        }
    }

    /// Get the widgets on a given side of a line.
    #[must_use] 
    pub fn widgets_on_side(&self, line_idx: usize, side: Side) -> &[StatusWidget] {
        self.lines.get(line_idx).map_or(&[], |line| match side {
            Side::Left => &line.left,
            Side::Right => &line.right,
        })
    }

    /// Set the separator character.
    pub fn set_separator(&mut self, sep: String) {
        self.separator_char = sep;
        self.preset = "custom".into();
    }

    /// Set compact mode.
    pub fn set_compact(&mut self, compact: bool) {
        self.compact = compact;
        self.preset = "custom".into();
    }

    /// Build a config from a named preset.
    #[must_use] 
    pub fn from_preset(preset: StatusLinePreset) -> Self {
        match preset {
            StatusLinePreset::Default => Self::preset_default(),
            StatusLinePreset::Minimal => Self::preset_minimal(),
            StatusLinePreset::Developer => Self::preset_developer(),
            StatusLinePreset::TokenHeavy => Self::preset_token_heavy(),
            StatusLinePreset::GitHeavy => Self::preset_git_heavy(),
            StatusLinePreset::Compact => Self::preset_compact(),
            StatusLinePreset::CostFocused => Self::preset_cost_focused(),
            StatusLinePreset::Streamer => Self::preset_streamer(),
            StatusLinePreset::Gaming => Self::preset_gaming(),
            StatusLinePreset::Devops => Self::preset_devops(),
            StatusLinePreset::BudgetTracker => Self::preset_budget_tracker(),
            StatusLinePreset::Zen => Self::preset_zen(),
            StatusLinePreset::Academic => Self::preset_academic(),
            StatusLinePreset::Hacker => Self::preset_hacker(),
            StatusLinePreset::NightOwl => Self::preset_night_owl(),
            StatusLinePreset::Dashboard => Self::preset_dashboard(),
        }
    }

    // ── Preset definitions ──────────────────────────────────────────────

    fn preset_default() -> Self {
        Self {
            preset: "default".into(),
            lines: vec![
                StatusLine {
                    left: vec![
                        StatusWidget::Model, StatusWidget::Separator,
                        StatusWidget::Thinking, StatusWidget::Separator,
                        StatusWidget::Cost, StatusWidget::Separator,
                        StatusWidget::GitBranch, StatusWidget::GitDiff,
                    ],
                    right: vec![StatusWidget::TokensTotal],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::ContextBar, StatusWidget::ContextTokens,
                        StatusWidget::Separator,
                        StatusWidget::SessionPct, StatusWidget::BlockTime,
                    ],
                    right: vec![StatusWidget::Version],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::Permissions, StatusWidget::Separator,
                        StatusWidget::QmdStatus, StatusWidget::ArchiveStatus,
                        StatusWidget::UpdateAvailable, StatusWidget::RemoteControl,
                    ],
                    right: vec![],
                },
            ],
            separator_char: " │ ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        }
    }

    fn preset_minimal() -> Self {
        Self {
            preset: "minimal".into(),
            lines: vec![
                StatusLine {
                    left: vec![StatusWidget::Model],
                    right: vec![StatusWidget::Cost],
                },
                StatusLine {
                    left: vec![StatusWidget::ContextBar, StatusWidget::ContextPct],
                    right: vec![StatusWidget::Version],
                },
            ],
            separator_char: " · ".into(),
            compact: true,
            widgets: std::collections::HashMap::new(),
        }
    }

    fn preset_developer() -> Self {
        Self {
            preset: "developer".into(),
            lines: vec![
                StatusLine {
                    left: vec![
                        StatusWidget::Model, StatusWidget::Separator,
                        StatusWidget::Thinking, StatusWidget::Separator,
                        StatusWidget::Cost,
                    ],
                    right: vec![StatusWidget::TokensTotal],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::ContextBar, StatusWidget::ContextTokens,
                        StatusWidget::Separator,
                        StatusWidget::SessionPct, StatusWidget::BlockTime,
                    ],
                    right: vec![StatusWidget::Version],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::Permissions, StatusWidget::Separator,
                        StatusWidget::GitBranch, StatusWidget::GitStatus,
                        StatusWidget::GitDiff, StatusWidget::Separator,
                        StatusWidget::QmdStatus, StatusWidget::ArchiveStatus,
                        StatusWidget::UpdateAvailable, StatusWidget::RemoteControl,
                    ],
                    right: vec![StatusWidget::VimMode],
                },
            ],
            separator_char: " │ ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        }
    }

    fn preset_token_heavy() -> Self {
        Self {
            preset: "token-heavy".into(),
            lines: vec![
                StatusLine {
                    left: vec![
                        StatusWidget::Model, StatusWidget::Separator,
                        StatusWidget::TokensInput, StatusWidget::Separator,
                        StatusWidget::TokensOutput, StatusWidget::Separator,
                        StatusWidget::TokensTotal, StatusWidget::Separator,
                        StatusWidget::Cost,
                    ],
                    right: vec![StatusWidget::TokenSpeed],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::ContextBar, StatusWidget::ContextTokens,
                        StatusWidget::ContextPct,
                    ],
                    right: vec![StatusWidget::Version],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::Permissions, StatusWidget::Separator,
                        StatusWidget::GitBranch,
                        StatusWidget::UpdateAvailable, StatusWidget::RemoteControl,
                    ],
                    right: vec![],
                },
            ],
            separator_char: " │ ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        }
    }

    fn preset_git_heavy() -> Self {
        Self {
            preset: "git-heavy".into(),
            lines: vec![
                StatusLine {
                    left: vec![
                        StatusWidget::GitBranch, StatusWidget::Separator,
                        StatusWidget::GitStatus, StatusWidget::Separator,
                        StatusWidget::GitDiff,
                    ],
                    right: vec![StatusWidget::Model],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::ContextBar, StatusWidget::ContextTokens,
                        StatusWidget::Separator, StatusWidget::Cost,
                    ],
                    right: vec![StatusWidget::Version],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::Permissions, StatusWidget::Separator,
                        StatusWidget::QmdStatus,
                        StatusWidget::UpdateAvailable, StatusWidget::RemoteControl,
                    ],
                    right: vec![],
                },
            ],
            separator_char: " │ ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        }
    }

    fn preset_compact() -> Self {
        Self {
            preset: "compact".into(),
            lines: vec![
                StatusLine {
                    left: vec![
                        StatusWidget::Model, StatusWidget::Separator,
                        StatusWidget::Thinking, StatusWidget::Separator,
                        StatusWidget::Cost, StatusWidget::Separator,
                        StatusWidget::GitBranch, StatusWidget::GitDiff,
                    ],
                    right: vec![StatusWidget::TokensTotal],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::ContextBar, StatusWidget::ContextPct,
                        StatusWidget::Separator,
                        StatusWidget::Permissions, StatusWidget::Separator,
                        StatusWidget::BlockTime,
                        StatusWidget::UpdateAvailable, StatusWidget::RemoteControl,
                    ],
                    right: vec![StatusWidget::Version],
                },
            ],
            separator_char: " · ".into(),
            compact: true,
            widgets: std::collections::HashMap::new(),
        }
    }

    fn preset_cost_focused() -> Self {
        Self {
            preset: "cost-focused".into(),
            lines: vec![
                StatusLine {
                    left: vec![
                        StatusWidget::Cost, StatusWidget::Separator,
                        StatusWidget::TokenSpeed, StatusWidget::Separator,
                        StatusWidget::Model,
                    ],
                    right: vec![
                        StatusWidget::TokensInput, StatusWidget::Separator,
                        StatusWidget::TokensOutput,
                    ],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::ContextBar, StatusWidget::ContextTokens,
                        StatusWidget::Separator, StatusWidget::SessionPct,
                    ],
                    right: vec![StatusWidget::Version],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::Permissions, StatusWidget::Separator,
                        StatusWidget::GitBranch,
                        StatusWidget::UpdateAvailable, StatusWidget::RemoteControl,
                    ],
                    right: vec![],
                },
            ],
            separator_char: " │ ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        }
    }

    fn preset_streamer() -> Self {
        Self {
            preset: "streamer".into(),
            lines: vec![
                StatusLine {
                    left: vec![StatusWidget::Model, StatusWidget::Separator, StatusWidget::Thinking],
                    right: vec![StatusWidget::GitBranch],
                },
                StatusLine {
                    left: vec![StatusWidget::ContextBar, StatusWidget::ContextPct],
                    right: vec![StatusWidget::Version],
                },
            ],
            separator_char: " │ ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        }
    }

    // ── Emoji-rich presets ───────────────────────────────────────────

    fn preset_gaming() -> Self {
        // 🎮 sonnet │ 🧠 Thinking: Yes │ 💰 $0.42
        // 🔥 $1.20/hr │ ⚡ 847 t/s │ 📊 [████████░░░░] 67%
        Self {
            preset: "gaming".into(),
            lines: vec![
                StatusLine {
                    left: vec![
                        StatusWidget::Model, StatusWidget::Separator,
                        StatusWidget::Thinking, StatusWidget::Separator,
                        StatusWidget::Cost,
                    ],
                    right: vec![StatusWidget::RemoteControl],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::BurnRate, StatusWidget::Separator,
                        StatusWidget::TokenSpeed, StatusWidget::Separator,
                        StatusWidget::ContextBar, StatusWidget::ContextPct,
                    ],
                    right: vec![StatusWidget::Version],
                },
            ],
            separator_char: " │ ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        }
    }

    fn preset_devops() -> Self {
        // 🐳 sonnet │ 🔧 Thinking │ 💵 $0.88 │ 🔀 main (+12,-5)
        // 📦 Context: [██████████░░] 83% │ 🔌 MCP: 3 │ 🛡️ workspace-write │ v2.2.2
        // 🛸 RC viewer.culpur.net [A7B3C2]
        Self {
            preset: "devops".into(),
            lines: vec![
                StatusLine {
                    left: vec![
                        StatusWidget::Model, StatusWidget::Separator,
                        StatusWidget::Thinking, StatusWidget::Separator,
                        StatusWidget::Cost, StatusWidget::Separator,
                        StatusWidget::GitBranch, StatusWidget::GitDiff,
                    ],
                    right: vec![StatusWidget::TokensTotal],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::ContextBar, StatusWidget::ContextTokens,
                        StatusWidget::Separator,
                        StatusWidget::McpStatus, StatusWidget::Separator,
                        StatusWidget::Permissions,
                    ],
                    right: vec![StatusWidget::Version],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::RemoteControl, StatusWidget::Separator,
                        StatusWidget::QmdStatus, StatusWidget::ArchiveStatus,
                        StatusWidget::UpdateAvailable,
                    ],
                    right: vec![StatusWidget::TimeDisplay],
                },
            ],
            separator_char: " │ ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        }
    }

    fn preset_budget_tracker() -> Self {
        // 💸 $0.42 │ 🔥 $1.20/hr │ 📈 Est: $4.80 │ 🎵 sonnet │ ⚡ 12.4K in / 3.2K out
        // 📊 [████░░░░░░░░] 33% │ 💰 Day: $2.10 │ 💰 Week: $14.30 │ 💰 Month: $89
        // 🛡️ permissions │ 🛸 RC status
        Self {
            preset: "budget-tracker".into(),
            lines: vec![
                StatusLine {
                    left: vec![
                        StatusWidget::Cost, StatusWidget::Separator,
                        StatusWidget::BurnRate, StatusWidget::Separator,
                        StatusWidget::CostProjection, StatusWidget::Separator,
                        StatusWidget::Model,
                    ],
                    right: vec![
                        StatusWidget::TokensInput, StatusWidget::Separator,
                        StatusWidget::TokensOutput,
                    ],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::ContextBar, StatusWidget::ContextPct,
                        StatusWidget::Separator,
                        StatusWidget::CostDaily, StatusWidget::Separator,
                        StatusWidget::CostWeekly, StatusWidget::Separator,
                        StatusWidget::CostMonthly,
                    ],
                    right: vec![StatusWidget::Version],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::Permissions, StatusWidget::Separator,
                        StatusWidget::RemoteControl,
                        StatusWidget::UpdateAvailable,
                    ],
                    right: vec![],
                },
            ],
            separator_char: " │ ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        }
    }

    fn preset_zen() -> Self {
        // 🎵 sonnet │ 🧘 45% │ v2.2.2
        Self {
            preset: "zen".into(),
            lines: vec![
                StatusLine {
                    left: vec![StatusWidget::Model],
                    right: vec![StatusWidget::ContextPct, StatusWidget::Separator, StatusWidget::Version],
                },
            ],
            separator_char: " │ ".into(),
            compact: true,
            widgets: std::collections::HashMap::new(),
        }
    }

    fn preset_academic() -> Self {
        // 📚 sonnet │ 🧠 Thinking │ 📝 +142/-38 lines │ ⏱️ 23m
        // 📊 [██████░░░░░░] 50% │ 🎓 32K tokens │ 🔖 v2.2.2
        Self {
            preset: "academic".into(),
            lines: vec![
                StatusLine {
                    left: vec![
                        StatusWidget::Model, StatusWidget::Separator,
                        StatusWidget::Thinking, StatusWidget::Separator,
                        StatusWidget::CodeProductivity, StatusWidget::Separator,
                        StatusWidget::SessionTime,
                    ],
                    right: vec![StatusWidget::Cost],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::ContextBar, StatusWidget::ContextTokens,
                        StatusWidget::Separator,
                        StatusWidget::TokensTotal,
                    ],
                    right: vec![StatusWidget::Version],
                },
            ],
            separator_char: " │ ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        }
    }

    fn preset_hacker() -> Self {
        // ⚡ sonnet │ 🔓 full-access │ 🔥 $2.10/hr │ 💀 95% ⚠️ CRITICAL
        // 🌐 ⌐main │ 📡 MCP: 5 │ 🛸 RC viewer [A7B3C2]
        Self {
            preset: "hacker".into(),
            lines: vec![
                StatusLine {
                    left: vec![
                        StatusWidget::Model, StatusWidget::Separator,
                        StatusWidget::Permissions, StatusWidget::Separator,
                        StatusWidget::BurnRate, StatusWidget::Separator,
                        StatusWidget::ContextPct,
                    ],
                    right: vec![StatusWidget::TokenSpeed],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::GitBranch, StatusWidget::GitDiff,
                        StatusWidget::Separator,
                        StatusWidget::McpStatus, StatusWidget::Separator,
                        StatusWidget::RemoteControl,
                    ],
                    right: vec![StatusWidget::UpdateAvailable],
                },
            ],
            separator_char: " │ ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        }
    }

    fn preset_night_owl() -> Self {
        // 🌙 sonnet │ ☕ Coding: 45m │ 🎵 $0.32
        // 🌊 [████████░░░░] 65% │ 🦉 v2.2.2
        Self {
            preset: "night-owl".into(),
            lines: vec![
                StatusLine {
                    left: vec![
                        StatusWidget::Model, StatusWidget::Separator,
                        StatusWidget::SessionTime, StatusWidget::Separator,
                        StatusWidget::Cost,
                    ],
                    right: vec![StatusWidget::TimeDisplay],
                },
                StatusLine {
                    left: vec![StatusWidget::ContextBar, StatusWidget::ContextPct],
                    right: vec![StatusWidget::Version],
                },
            ],
            separator_char: " │ ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        }
    }

    fn preset_dashboard() -> Self {
        // 🎵 sonnet │ 🧠 Yes │ 💰 $0.88 │ 🔀 main │ 📝 (+12,-5)
        // 📊 [████████░░░░] 67% 134K/200K │ ⏱️ 23m │ 🔥 $1.80/hr │ ⚡ 847 t/s
        // 🛡️ workspace-write │ 📚 QMD: 42 docs │ 📦 Archive: 5 │ 🔌 MCP: 3
        // 💰 Day: $3.20 │ 💰 Week: $18.50 │ 📈 Est: $6.40 │ 📝 +258/-94 │ 🛸 RC
        Self {
            preset: "dashboard".into(),
            lines: vec![
                StatusLine {
                    left: vec![
                        StatusWidget::Model, StatusWidget::Separator,
                        StatusWidget::Thinking, StatusWidget::Separator,
                        StatusWidget::Cost, StatusWidget::Separator,
                        StatusWidget::GitBranch, StatusWidget::GitDiff,
                    ],
                    right: vec![StatusWidget::TokensTotal],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::ContextBar, StatusWidget::ContextTokens,
                        StatusWidget::Separator,
                        StatusWidget::SessionTime, StatusWidget::Separator,
                        StatusWidget::BurnRate, StatusWidget::Separator,
                        StatusWidget::TokenSpeed,
                    ],
                    right: vec![StatusWidget::Version],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::Permissions, StatusWidget::Separator,
                        StatusWidget::QmdStatus, StatusWidget::ArchiveStatus,
                        StatusWidget::Separator,
                        StatusWidget::McpStatus,
                    ],
                    right: vec![StatusWidget::TimeDisplay],
                },
                StatusLine {
                    left: vec![
                        StatusWidget::CostDaily, StatusWidget::Separator,
                        StatusWidget::CostWeekly, StatusWidget::Separator,
                        StatusWidget::CostProjection, StatusWidget::Separator,
                        StatusWidget::CodeProductivity, StatusWidget::Separator,
                        StatusWidget::RemoteControl,
                    ],
                    right: vec![StatusWidget::UpdateAvailable],
                },
            ],
            separator_char: " │ ".into(),
            compact: false,
            widgets: std::collections::HashMap::new(),
        }
    }
}

impl Default for StatusLineConfig {
    fn default() -> Self {
        Self::preset_default()
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_names_round_trip() {
        for name in Theme::builtin_names() {
            let t = Theme::builtin(name).expect("should resolve");
            assert_eq!(&t.name, name);
        }
    }

    #[test]
    fn default_theme_is_culpur_defense() {
        assert_eq!(Theme::default_theme().name, "culpur-defense");
    }

    #[test]
    fn rgb_hex_round_trip() {
        let c = Rgb(0x0F, 0xBC, 0xFF);
        let hex = c.to_hex();
        let parsed = Rgb::from_hex(&hex).unwrap();
        assert_eq!(c, parsed);
    }

    #[test]
    fn unknown_builtin_returns_none() {
        assert!(Theme::builtin("does-not-exist").is_none());
    }

    // ── Status line tests ───────────────────────────────────────────────

    #[test]
    fn all_presets_round_trip() {
        for preset in StatusLinePreset::all() {
            let cfg = StatusLineConfig::from_preset(*preset);
            assert_eq!(cfg.preset, preset.name());
            assert!(!cfg.lines.is_empty(), "preset {} has no lines", preset.name());
            // Serialize and deserialize
            let json = serde_json::to_string(&cfg).expect("serialize");
            let back: StatusLineConfig = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back.preset, preset.name());
            assert_eq!(back.lines.len(), cfg.lines.len());
        }
    }

    #[test]
    fn preset_from_name_round_trip() {
        for preset in StatusLinePreset::all() {
            let found = StatusLinePreset::from_name(preset.name());
            assert_eq!(found, Some(*preset));
        }
        assert_eq!(StatusLinePreset::from_name("nonexistent"), None);
    }

    #[test]
    fn default_config_has_3_lines() {
        let cfg = StatusLineConfig::default();
        assert_eq!(cfg.line_count(), 3);
        assert_eq!(cfg.preset, "default");
    }

    #[test]
    fn compact_presets_have_2_lines() {
        let minimal = StatusLineConfig::from_preset(StatusLinePreset::Minimal);
        assert_eq!(minimal.line_count(), 2);
        let compact = StatusLineConfig::from_preset(StatusLinePreset::Compact);
        assert_eq!(compact.line_count(), 2);
        let streamer = StatusLineConfig::from_preset(StatusLinePreset::Streamer);
        assert_eq!(streamer.line_count(), 2);
    }

    #[test]
    fn widget_id_unique() {
        let widgets = vec![
            StatusWidget::Model, StatusWidget::Thinking, StatusWidget::Effort,
            StatusWidget::Provider, StatusWidget::TokensTotal, StatusWidget::TokensInput,
            StatusWidget::TokensOutput, StatusWidget::Cost, StatusWidget::TokenSpeed,
            StatusWidget::ContextBar, StatusWidget::ContextPct, StatusWidget::ContextTokens,
            StatusWidget::SessionTime, StatusWidget::SessionPct, StatusWidget::BlockTime,
            StatusWidget::GitBranch, StatusWidget::GitStatus, StatusWidget::GitDiff,
            StatusWidget::Permissions, StatusWidget::QmdStatus, StatusWidget::Version,
            StatusWidget::VimMode, StatusWidget::RemoteControl, StatusWidget::UpdateAvailable,
            StatusWidget::ArchiveStatus, StatusWidget::McpStatus, StatusWidget::TimeDisplay,
            StatusWidget::BurnRate, StatusWidget::CostDaily, StatusWidget::CostWeekly,
            StatusWidget::CostMonthly, StatusWidget::CostProjection, StatusWidget::CacheHitRate,
            StatusWidget::CodeProductivity, StatusWidget::Spacer, StatusWidget::Separator,
        ];
        let mut ids: Vec<&str> = widgets.iter().map(super::StatusWidget::id).collect();
        let len_before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), len_before, "widget IDs must be unique");
    }

    #[test]
    fn all_widgets_count() {
        let all = StatusWidget::all_widgets();
        assert_eq!(all.len(), 36, "expected 36 concrete widget variants (excluding Text)");
    }

    #[test]
    fn from_id_round_trip() {
        for w in StatusWidget::all_widgets() {
            let id = w.id();
            let parsed = StatusWidget::from_id(id);
            assert!(parsed.is_some(), "from_id failed for: {id}");
            assert_eq!(parsed.unwrap().id(), id, "round-trip failed for: {id}");
        }
    }

    #[test]
    fn category_returns_known_values() {
        let known = ["model", "tokens", "context", "session", "git", "system", "cost_detail", "productivity", "layout"];
        for w in StatusWidget::all_widgets() {
            assert!(known.contains(&w.category()), "unknown category '{}' for widget '{}'", w.category(), w.id());
        }
    }

    #[test]
    fn mutation_add_remove_widget() {
        let mut cfg = StatusLineConfig::default();
        let original_count = cfg.widgets_on_side(0, Side::Left).len();
        cfg.add_widget(0, Side::Left, StatusWidget::TimeDisplay);
        assert_eq!(cfg.widgets_on_side(0, Side::Left).len(), original_count + 1);
        assert_eq!(cfg.preset, "custom");
        cfg.remove_widget(0, Side::Left, original_count);
        assert_eq!(cfg.widgets_on_side(0, Side::Left).len(), original_count);
    }

    #[test]
    fn mutation_move_widget() {
        let mut cfg = StatusLineConfig::from_preset(StatusLinePreset::Default);
        let first = cfg.widgets_on_side(0, Side::Left)[0].id().to_string();
        let second = cfg.widgets_on_side(0, Side::Left)[1].id().to_string();
        cfg.move_widget(0, Side::Left, 0, 1);
        assert_eq!(cfg.widgets_on_side(0, Side::Left)[0].id(), second);
        assert_eq!(cfg.widgets_on_side(0, Side::Left)[1].id(), first);
    }

    #[test]
    fn mutation_add_remove_line() {
        let mut cfg = StatusLineConfig::default();
        let original = cfg.line_count();
        cfg.add_line();
        assert_eq!(cfg.line_count(), original + 1);
        cfg.remove_line(original);
        assert_eq!(cfg.line_count(), original);
        // Cannot remove last line
        let mut single = StatusLineConfig::from_preset(StatusLinePreset::Zen);
        assert_eq!(single.line_count(), 1);
        single.remove_line(0);
        assert_eq!(single.line_count(), 1, "should not remove the last line");
    }
}
