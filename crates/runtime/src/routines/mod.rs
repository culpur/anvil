/// On-disk foundation for scheduled routine runs.
///
/// This module provides:
/// - [`SILENT_MARKER`] — the output token that suppresses delivery
/// - [`is_silent_output`] — check whether an LLM response should be archived
///   locally but not forwarded to any delivery target
/// - [`schedule`] — schedule expression parsing and next-fire computation
/// - [`archive`] — per-run markdown archive on disk
/// - [`packet`] — run output packet schema with input hash and injection delimiters
pub mod archive;
pub mod definition;
pub mod delivery;
pub mod executor;
pub mod packet;
pub mod proposal;
pub mod schedule;

/// When an LLM response contains this exact string the dispatcher writes the
/// run to the local archive but skips all delivery targets (email, webhook,
/// chat, etc.).  The check is case-sensitive.
pub const SILENT_MARKER: &str = "[SILENT]";

/// Return `true` when `output` contains [`SILENT_MARKER`] (case-sensitive).
///
/// The caller should write the archive entry as normal but skip every delivery
/// target when this function returns `true`.
#[must_use]
pub fn is_silent_output(output: &str) -> bool {
    output.contains(SILENT_MARKER)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_marker_at_end() {
        assert!(is_silent_output("All good. [SILENT]"));
    }

    #[test]
    fn no_marker_returns_false() {
        assert!(!is_silent_output("All good."));
    }

    #[test]
    fn lowercase_marker_not_matched() {
        assert!(!is_silent_output("[silent]"));
    }

    #[test]
    fn empty_string_returns_false() {
        assert!(!is_silent_output(""));
    }
}
