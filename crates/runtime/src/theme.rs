/// Theme system for Anvil TUI.
///
/// A `Theme` holds a palette of RGB triples for every named slot used by the
/// TUI.  The current theme is persisted to `~/.anvil/theme.json` so it
/// survives across sessions.  Five built-in themes are provided; users can
/// also hand-edit the JSON to create custom palettes.
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
}
