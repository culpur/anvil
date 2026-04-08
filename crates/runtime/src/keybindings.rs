use serde::{Deserialize, Serialize};

use crate::config::default_config_home;

/// User-configurable key bindings for the TUI.
///
/// Each field holds a list of key chord strings in the form `"Ctrl+V"`,
/// `"Shift+Enter"`, `"Escape"`, etc.  Multiple chords per action allow
/// the user to define aliases.
///
/// The file is loaded from `~/.anvil/keybindings.json` when present.
/// Missing or invalid fields fall back to the hardcoded defaults below.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeybindingsConfig {
    /// Submit the current input to the model.
    #[serde(default = "default_submit")]
    pub submit: Vec<String>,
    /// Cancel the current operation / close a popup.
    #[serde(default = "default_cancel")]
    pub cancel: Vec<String>,
    /// Insert a newline without submitting.
    #[serde(default = "default_new_line")]
    pub new_line: Vec<String>,
    /// Toggle Vim-mode input editing.
    #[serde(default = "default_toggle_vim")]
    pub toggle_vim: Vec<String>,
    /// Open / close the agents panel.
    #[serde(default = "default_toggle_agents")]
    pub toggle_agents: Vec<String>,
}

// ─── Defaults ──────────────────────────────────────────────────────────────

fn default_submit() -> Vec<String> {
    vec!["Enter".to_string()]
}

fn default_cancel() -> Vec<String> {
    vec!["Escape".to_string()]
}

fn default_new_line() -> Vec<String> {
    vec!["Shift+Enter".to_string()]
}

fn default_toggle_vim() -> Vec<String> {
    vec!["Ctrl+V".to_string()]
}

fn default_toggle_agents() -> Vec<String> {
    vec!["Ctrl+A".to_string()]
}

// ─── Implementation ────────────────────────────────────────────────────────

impl Default for KeybindingsConfig {
    fn default() -> Self {
        Self {
            submit: default_submit(),
            cancel: default_cancel(),
            new_line: default_new_line(),
            toggle_vim: default_toggle_vim(),
            toggle_agents: default_toggle_agents(),
        }
    }
}

impl KeybindingsConfig {
    /// Load keybindings from `~/.anvil/keybindings.json`.
    ///
    /// Returns `Self::default()` if the file does not exist or cannot be
    /// parsed.
    #[must_use]
    pub fn load() -> Self {
        let path = default_config_home().join("keybindings.json");
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Return `true` if `chord` is bound to the `submit` action.
    #[must_use]
    pub fn is_submit(&self, chord: &str) -> bool {
        self.submit.iter().any(|k| k == chord)
    }

    /// Return `true` if `chord` is bound to the `cancel` action.
    #[must_use]
    pub fn is_cancel(&self, chord: &str) -> bool {
        self.cancel.iter().any(|k| k == chord)
    }

    /// Return `true` if `chord` is bound to the `new_line` action.
    #[must_use]
    pub fn is_new_line(&self, chord: &str) -> bool {
        self.new_line.iter().any(|k| k == chord)
    }

    /// Return `true` if `chord` is bound to the `toggle_vim` action.
    #[must_use]
    pub fn is_toggle_vim(&self, chord: &str) -> bool {
        self.toggle_vim.iter().any(|k| k == chord)
    }

    /// Return `true` if `chord` is bound to the `toggle_agents` action.
    #[must_use]
    pub fn is_toggle_agents(&self, chord: &str) -> bool {
        self.toggle_agents.iter().any(|k| k == chord)
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bindings_are_sensible() {
        let kb = KeybindingsConfig::default();
        assert!(kb.is_submit("Enter"));
        assert!(kb.is_cancel("Escape"));
        assert!(kb.is_new_line("Shift+Enter"));
        assert!(kb.is_toggle_vim("Ctrl+V"));
        assert!(kb.is_toggle_agents("Ctrl+A"));
    }

    #[test]
    fn unknown_chord_is_not_bound() {
        let kb = KeybindingsConfig::default();
        assert!(!kb.is_submit("Ctrl+Q"));
        assert!(!kb.is_cancel("Enter"));
    }

    #[test]
    fn custom_bindings_from_json() {
        let json = r#"{"submit": ["Ctrl+Enter"], "cancel": ["Escape"], "new_line": ["Alt+Enter"], "toggle_vim": ["Ctrl+V"], "toggle_agents": ["Ctrl+A"]}"#;
        let kb: KeybindingsConfig = serde_json::from_str(json).unwrap();
        assert!(kb.is_submit("Ctrl+Enter"));
        assert!(!kb.is_submit("Enter"));
    }
}
