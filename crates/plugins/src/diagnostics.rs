use std::path::PathBuf;

// ---------------------------------------------------------------------------
// PluginLoadDiagnostic
// ---------------------------------------------------------------------------

/// A per-plugin warning collected during plugin discovery.
///
/// These are non-fatal: discovery continues for all other plugins.  Callers
/// surface them to the user via `PluginManager::take_diagnostics()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginLoadDiagnostic {
    /// `plugin.json` (or `.anvil-plugin/plugin.json`) was not found.
    ManifestMissing { dir: PathBuf },

    /// The manifest file exists but could not be parsed or validated.
    ManifestParse { dir: PathBuf, detail: String },

    /// A single hook entry in a `PreToolUse` / `PostToolUse` array had an
    /// unrecognized shape and was dropped.  The rest of the array still loads.
    UnknownHookVariant {
        plugin: String,
        event: String,
        index: usize,
        detail: String,
    },
}

impl PluginLoadDiagnostic {
    /// A short deduplification key used to suppress repeated identical warnings
    /// within a single process lifetime.
    #[must_use]
    pub fn dedup_key(&self) -> String {
        match self {
            Self::ManifestMissing { dir } => {
                format!("missing:{}", dir.display())
            }
            Self::ManifestParse { dir, .. } => {
                format!("parse:{}", dir.display())
            }
            Self::UnknownHookVariant {
                plugin,
                event,
                index,
                ..
            } => {
                format!("hook-variant:{plugin}:{event}:{index}")
            }
        }
    }

    /// A human-readable warning line suitable for printing to stderr.
    ///
    /// Hook bodies are intentionally absent from this output; only plugin
    /// names, event names, indices, and sanitized error details are included.
    #[must_use]
    pub fn display_line(&self) -> String {
        match self {
            Self::ManifestMissing { dir } => {
                format!(
                    "[plugin warning] no manifest found in {}",
                    dir.display()
                )
            }
            Self::ManifestParse { dir, detail } => {
                format!(
                    "[plugin warning] skipped {}: manifest parse error ({})",
                    dir.display(),
                    truncate_detail(detail)
                )
            }
            Self::UnknownHookVariant {
                plugin,
                event,
                index,
                detail,
            } => {
                format!(
                    "[plugin warning] skipped {plugin}: unrecognized hook variant at {event}[{index}] (requires a newer anvil? {})",
                    truncate_detail(detail)
                )
            }
        }
    }
}

/// Truncate a detail string to 80 characters so hook bodies with interpolation
/// tokens are not accidentally broadcast to stderr in full.
fn truncate_detail(detail: &str) -> String {
    // Work in chars to avoid splitting multi-byte codepoints.
    let mut chars = detail.chars();
    let truncated: String = chars.by_ref().take(80).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_key_is_stable() {
        let d = PluginLoadDiagnostic::ManifestParse {
            dir: PathBuf::from("/tmp/broken"),
            detail: "expected value at line 1".to_string(),
        };
        assert_eq!(d.dedup_key(), "parse:/tmp/broken");

        let d2 = PluginLoadDiagnostic::UnknownHookVariant {
            plugin: "my-plugin".to_string(),
            event: "PreToolUse".to_string(),
            index: 0,
            detail: "invalid type: map".to_string(),
        };
        assert_eq!(d2.dedup_key(), "hook-variant:my-plugin:PreToolUse:0");
    }

    #[test]
    fn display_line_truncates_long_detail() {
        let long_detail = "x".repeat(120);
        let d = PluginLoadDiagnostic::ManifestParse {
            dir: PathBuf::from("/tmp/x"),
            detail: long_detail,
        };
        let line = d.display_line();
        // The detail portion should be truncated with "..." inside the parens.
        assert!(line.contains("..."), "expected truncation: {line}");
        // Total detail portion must not exceed 80 chars + "..."
        assert!(line.len() < 200, "line too long: {line}");
    }
}
