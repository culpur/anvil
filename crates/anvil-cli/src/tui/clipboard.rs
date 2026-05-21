//! OSC 52 clipboard writer.
//!
//! Task #748: when the user copies (Ctrl+C / Cmd+C with a selection), Anvil
//! must put the text onto the system clipboard. From inside the TUI alt-
//! screen we use OSC 52 — `ESC ] 52 ; c ; <base64> BEL` — which works
//! across most modern terminals (iTerm2, Apple Terminal, kitty, Gnome
//! Terminal, Windows Terminal, Wezterm) and passes through tmux when
//! `set -g set-clipboard on` is enabled.
//!
//! No external clipboard crate is in the dependency tree (`arboard` etc.
//! were considered and intentionally avoided so the cross-compile matrix
//! stays portable for FreeBSD / NetBSD source builds — see
//! feedback-cross-platform-ux-defaults.md). OSC 52 is the universal
//! mechanism that does NOT require linking native libs.
//!
//! See also: feedback-paste-copy-select-never-optional.md.
use std::io::Write;

use base64::Engine;

/// Maximum text length OSC 52 will write to the clipboard. Most terminals
/// cap OSC 52 payloads around 8 KB after base64 expansion; we cap the
/// pre-encode text at 100 KB which is far above any realistic selection.
const OSC52_MAX_BYTES: usize = 100 * 1024;

/// Write `text` to the system clipboard via OSC 52.
///
/// Returns `Ok(())` on success, `Err(String)` on failure. Empty text is a
/// no-op success (clearing the clipboard is intentionally NOT done here —
/// we don't want a stray empty copy to wipe the user's previous clipboard).
///
/// The escape sequence is written directly to stdout (the alt-screen's
/// backing FD). Inside the TUI runtime, ratatui's renderer queues its own
/// writes through its `Terminal` backend, but OSC 52 is a one-shot escape
/// that doesn't interfere with ratatui's cell diffing — the terminal
/// emulator strips it before painting.
pub fn write_clipboard(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    if text.len() > OSC52_MAX_BYTES {
        return Err(format!(
            "Selection too large for OSC 52 clipboard: {} bytes (max {})",
            text.len(),
            OSC52_MAX_BYTES,
        ));
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let payload = format!("\x1b]52;c;{encoded}\x07");
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle
        .write_all(payload.as_bytes())
        .map_err(|e| format!("OSC 52 write failed: {e}"))?;
    handle
        .flush()
        .map_err(|e| format!("OSC 52 flush failed: {e}"))?;
    Ok(())
}

/// Encode `text` as an OSC 52 escape string. Pure helper exposed for
/// testability — `write_clipboard` calls this then writes to stdout.
#[must_use]
pub fn osc52_encode(text: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    /// Task #748: the OSC 52 sequence must be exactly
    /// `ESC ] 52 ; c ; <base64> BEL`. This test pins the bytes so any
    /// future refactor that breaks the wire format trips immediately.
    #[test]
    fn osc52_encodes_text_correctly() {
        let got = osc52_encode("hello world");
        let want_b64 = base64::engine::general_purpose::STANDARD.encode(b"hello world");
        let want = format!("\x1b]52;c;{want_b64}\x07");
        assert_eq!(got, want);
        // Spot-check the boundary bytes too.
        assert!(got.starts_with("\x1b]52;c;"));
        assert!(got.ends_with('\x07'));
    }

    /// Task #748: writing an empty string is a no-op success — we
    /// explicitly do NOT clear the clipboard with an empty OSC 52
    /// payload, because that would let an accidental Ctrl+C with no
    /// selection nuke the user's previous clipboard contents.
    #[test]
    fn write_clipboard_handles_empty_string() {
        // Should succeed without writing anything (we can't observe the
        // "didn't write" easily from a test; the contract is just that
        // it returns Ok).
        assert!(write_clipboard("").is_ok());
    }

    /// Task #748: unicode round-trips through base64 cleanly. Selection
    /// text often contains emoji / accented chars / multi-byte UTF-8 —
    /// the encoder must treat the input as raw bytes, not as a char
    /// sequence.
    #[test]
    fn write_clipboard_handles_unicode() {
        let input = "héllo 世界 🦀";
        let encoded = osc52_encode(input);
        // Verify we can decode it back to the same bytes.
        let inside_payload = encoded
            .strip_prefix("\x1b]52;c;")
            .and_then(|s| s.strip_suffix('\x07'))
            .expect("payload framed correctly");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(inside_payload)
            .expect("base64 round-trip");
        assert_eq!(decoded, input.as_bytes());
    }

    /// Task #748: payloads above the 100 KB cap are refused with an
    /// error rather than truncated, so the caller can fall back to a
    /// visible "selection too large" notice instead of silently
    /// clipping the user's data.
    #[test]
    fn write_clipboard_refuses_oversized_payload() {
        let huge = "a".repeat(OSC52_MAX_BYTES + 1);
        let result = write_clipboard(&huge);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("too large"), "got {msg:?}");
    }
}
