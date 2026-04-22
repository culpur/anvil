use serde::{Deserialize, Serialize};

/// User-selectable response style.
///
/// `Precise` is the default and matches the model's natural voice.
/// `Condensed` prepends the bundled `terse` skill body to every system prompt
/// turn, activating token-economical response rules with Auto-Clarity.
///
/// Policy (2026-04-22):
/// - `Precise` is ALWAYS the default. A user who has never set `output_style`
///   gets `Precise`.
/// - `Condensed` is strictly opt-in; it is never auto-applied from trigger
///   keywords or heuristics.
/// - This axis is orthogonal to `/skill load terse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutputStyle {
    /// Natural model voice — no extra instructions prepended. (default)
    #[default]
    Precise,
    /// Token-economical mode — the bundled `terse` skill body is prepended to
    /// the system prompt for every turn. Auto-Clarity rules still apply.
    Condensed,
}

impl OutputStyle {
    /// The canonical lowercase string representation used in config.json.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Precise => "precise",
            Self::Condensed => "condensed",
        }
    }

    /// Parse a string into an `OutputStyle`. Case-insensitive.
    /// Returns `None` for unknown values.
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "precise" => Some(Self::Precise),
            "condensed" => Some(Self::Condensed),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OutputStyle;
    use serde_json;

    #[test]
    fn default_is_precise() {
        assert_eq!(OutputStyle::default(), OutputStyle::Precise);
    }

    #[test]
    fn from_str_parses_lowercase() {
        assert_eq!(OutputStyle::from_str("precise"), Some(OutputStyle::Precise));
        assert_eq!(OutputStyle::from_str("condensed"), Some(OutputStyle::Condensed));
    }

    #[test]
    fn from_str_parses_uppercase() {
        assert_eq!(OutputStyle::from_str("PRECISE"), Some(OutputStyle::Precise));
        assert_eq!(OutputStyle::from_str("CONDENSED"), Some(OutputStyle::Condensed));
    }

    #[test]
    fn from_str_rejects_unknown() {
        assert_eq!(OutputStyle::from_str("foo"), None);
        assert_eq!(OutputStyle::from_str("ultra"), None);
        assert_eq!(OutputStyle::from_str(""), None);
    }

    #[test]
    fn json_round_trip() {
        let serialized = serde_json::to_string(&OutputStyle::Condensed).unwrap();
        assert_eq!(serialized, "\"condensed\"");
        let deserialized: OutputStyle = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, OutputStyle::Condensed);

        let serialized_precise = serde_json::to_string(&OutputStyle::Precise).unwrap();
        assert_eq!(serialized_precise, "\"precise\"");
        let deserialized_precise: OutputStyle = serde_json::from_str(&serialized_precise).unwrap();
        assert_eq!(deserialized_precise, OutputStyle::Precise);
    }
}
