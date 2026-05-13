//! Embedded release notes for the current binary.
//!
//! End users only ship with the `anvil` binary — they do NOT have the
//! `RELEASE-NOTES-vX.Y.Z.md` files in their cwd. So we bake the notes for the
//! currently-building version into the binary at compile time via `include_str!`
//! and `build.rs` (which copies `RELEASE-NOTES-v{CARGO_PKG_VERSION}.md` into
//! `OUT_DIR/release_notes.md`).
//!
//! Three surfaces consume this:
//!   1. `prompt.rs::environment_section()` injects [`headline()`] into the
//!      system prompt every turn so the model has zero-step self-knowledge.
//!   2. The `/changelog` slash command prints [`FULL_TEXT`] to the TUI.
//!   3. The `read_release_notes` tool returns [`FULL_TEXT`] when the model asks.

/// Full embedded release notes for `CARGO_PKG_VERSION`.
///
/// Generated at build time by `crates/runtime/build.rs` from the workspace-root
/// file `RELEASE-NOTES-v{version}.md`. Falls back to a placeholder if the file
/// is missing at build time (e.g., dev build between a version bump and
/// notes-being-written).
pub const FULL_TEXT: &str = include_str!(concat!(env!("OUT_DIR"), "/release_notes.md"));

/// The version string this binary was built for (`CARGO_PKG_VERSION`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// One-line headline extracted from the release notes (the H1 title, with the
/// leading `# ` stripped).
///
/// Returns `"Anvil v{VERSION}"` if no `# ` line is present.
pub fn headline() -> String {
    FULL_TEXT
        .lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim).map(str::to_string))
        .unwrap_or_else(|| format!("Anvil v{VERSION}"))
}

/// Extracts the lead paragraph after the H1 — the first non-blank, non-heading,
/// non-metadata block of text. Used by the env-block summary so the model has
/// the "why" of the release without forcing a tool call.
///
/// Skips:
///   - The H1 line itself
///   - "Released: ..." metadata lines
///   - Horizontal rules (`---`)
///   - Sub-headings (`## ...`)
/// Stops at the first blank line after capturing content.
pub fn lead_paragraph() -> String {
    let mut buf = String::new();
    let mut started = false;
    for raw in FULL_TEXT.lines() {
        let line = raw.trim_end();
        if line.starts_with('#') {
            if started {
                break;
            }
            continue;
        }
        if line.starts_with("Released:") || line.trim() == "---" {
            continue;
        }
        if line.trim().is_empty() {
            if started {
                break;
            }
            continue;
        }
        if started {
            buf.push(' ');
        }
        buf.push_str(line.trim());
        started = true;
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_text_is_non_empty() {
        assert!(!FULL_TEXT.trim().is_empty(), "embedded release notes must not be empty");
    }

    #[test]
    fn version_matches_cargo_pkg() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn headline_is_non_empty() {
        let h = headline();
        assert!(!h.is_empty());
        assert!(h.starts_with("Anvil "), "headline should start with 'Anvil ', got: {h}");
    }

    #[test]
    fn lead_paragraph_is_non_empty() {
        let p = lead_paragraph();
        assert!(!p.is_empty(), "lead paragraph must not be empty");
        // Lead paragraph must not contain markdown heading markers
        assert!(!p.contains('#'), "lead paragraph should not contain # heading markers: {p}");
    }
}
