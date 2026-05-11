/// Routine run output packet schema.
///
/// A `RoutinePacket` is an immutable record of one routine run's output.  It
/// is stored as a JSON sidecar next to the markdown archive file and can be
/// injected into downstream routine prompts using [`wrap_for_injection`].
///
/// The [`PACKET_OPEN`] / [`PACKET_CLOSE`] delimiters prevent a malicious
/// upstream routine from injecting prompt-control tokens into downstream
/// context by stripping any pre-existing occurrences of the delimiters from
/// the body before wrapping.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::routines::archive::archive_path;

/// Monotonic counter for unique tmp filenames.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Delimiter constants
// ---------------------------------------------------------------------------

/// Opening delimiter used when injecting a packet body into a downstream prompt.
pub const PACKET_OPEN: &str = "<<<ROUTINE-PACKET-START>>>";

/// Closing delimiter used when injecting a packet body into a downstream prompt.
pub const PACKET_CLOSE: &str = "<<<ROUTINE-PACKET-END>>>";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Status of a routine run as stored in the packet.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PacketStatus {
    Clean,
    Silent,
    Failed,
}

/// Immutable output record for a single routine run, used as upstream context
/// for chained routines.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutinePacket {
    /// Unique ID of the routine definition.
    pub routine_id: String,
    /// 8-character run identifier.
    pub run_id: String,
    /// Unix seconds when execution started.
    pub started_at: u64,
    /// Unix seconds when execution ended.
    pub ended_at: u64,
    /// Run outcome.
    pub status: PacketStatus,
    /// First paragraph of the body, max 280 chars.  Empty for Silent/Failed.
    pub summary: String,
    /// Full LLM output text, verbatim.
    pub body: String,
    /// Lowercase hex SHA-256 of the concatenated inputs.
    pub input_hash: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compute the SHA-256 input hash for a packet.
///
/// Components are joined with NUL bytes so that distinct inputs cannot be made
/// hash-equivalent by shifting content across field boundaries.
pub fn compute_input_hash(
    system_prompt: &str,
    user_prompt: &str,
    script_output: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(system_prompt.as_bytes());
    hasher.update(b"\x00");
    hasher.update(user_prompt.as_bytes());
    hasher.update(b"\x00");
    if let Some(so) = script_output {
        hasher.update(so.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Extract a summary from a body string.
///
/// Returns the first paragraph (text before the first blank line), truncated
/// to at most 280 characters with a word-boundary cut and a `…` suffix.
/// Returns `""` for empty bodies or bodies that consist solely of the silent
/// marker token.
#[must_use]
pub fn extract_summary(body: &str) -> String {
    use crate::routines::SILENT_MARKER;

    let trimmed = body.trim();
    if trimmed.is_empty() || trimmed == SILENT_MARKER {
        return String::new();
    }

    // First paragraph = text up to the first blank line.
    let first_para = trimmed.split("\n\n").next().unwrap_or(trimmed).trim();

    truncate_summary(first_para, 280)
}

/// Truncate `s` to at most `max_chars` unicode scalar values.
///
/// Tries to cut at the last ASCII space before the limit; if no space is found
/// the cut is made at the character boundary.  A `…` suffix is appended when
/// truncation occurs.
fn truncate_summary(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }

    // Collect up to max_chars chars + enough to check for a word boundary.
    let chars: Vec<char> = s.chars().take(max_chars).collect();
    let prefix: String = chars.iter().collect();

    // Try to cut at last space.
    if let Some(cut) = prefix.rfind(' ') {
        format!("{}…", &prefix[..cut])
    } else {
        format!("{prefix}…")
    }
}

/// Write a JSON sidecar packet alongside the markdown archive file.
///
/// Path: `~/.anvil/routines/output/{routine_id}/{ISO}.json`
pub fn write_packet(packet: &RoutinePacket) -> Result<PathBuf, String> {
    // Derive the sidecar path from the archive md path.
    let md_path = archive_path(&packet.routine_id, packet.started_at)?;
    let target = md_path.with_extension("json");

    let parent = target
        .parent()
        .ok_or_else(|| format!("no parent directory for packet path: {}", target.display()))?;

    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create packet dir `{}`: {e}", parent.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(parent, perms)
            .map_err(|e| format!("cannot set permissions on `{}`: {e}", parent.display()))?;
    }

    let json = serde_json::to_string_pretty(packet)
        .map_err(|e| format!("cannot serialise packet: {e}"))?;

    // Atomic write.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        "{}.tmp.{pid}.{nanos:010}.{seq}",
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("packet")
    );
    let tmp = parent.join(tmp_name);

    if let Err(e) = std::fs::write(&tmp, &json) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot write packet tmp file: {e}"));
    }

    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot rename packet tmp to target: {e}")
    })?;

    Ok(target)
}

/// Wrap a packet body for safe injection into a downstream routine's prompt.
///
/// Any pre-existing occurrences of [`PACKET_OPEN`] or [`PACKET_CLOSE`] in
/// `body` are stripped before wrapping to prevent delimiter injection from a
/// malicious upstream.
pub fn wrap_for_injection(body: &str, routine_name: &str) -> String {
    let clean_body = body.replace(PACKET_OPEN, "").replace(PACKET_CLOSE, "");

    format!(
        "{open} name={name}>\n{body}\n{close} name={name}>",
        open = PACKET_OPEN,
        close = PACKET_CLOSE,
        name = routine_name,
        body = clean_body,
    )
}

// ---------------------------------------------------------------------------
// Private helper re-exported for test visibility
// ---------------------------------------------------------------------------

#[cfg(test)]
fn packet_path_for(routine_id: &str, started_at: u64) -> Result<PathBuf, String> {
    let md = archive_path(routine_id, started_at)?;
    Ok(md.with_extension("json"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_packet() -> RoutinePacket {
        RoutinePacket {
            routine_id: "rtn-abc12345".to_string(),
            run_id: "run00001".to_string(),
            started_at: 946_684_800,
            ended_at: 946_684_860,
            status: PacketStatus::Clean,
            summary: "Everything looks good.".to_string(),
            body: "Everything looks good.\n\nSecond paragraph.".to_string(),
            input_hash: compute_input_hash("sys", "usr", None),
        }
    }

    // ── compute_input_hash ────────────────────────────────────────────────

    #[test]
    fn hash_is_deterministic() {
        let h1 = compute_input_hash("sys", "usr", Some("out"));
        let h2 = compute_input_hash("sys", "usr", Some("out"));
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_differs_on_system_prompt_change() {
        let h1 = compute_input_hash("sys-a", "usr", None);
        let h2 = compute_input_hash("sys-b", "usr", None);
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_differs_on_user_prompt_change() {
        let h1 = compute_input_hash("sys", "usr-a", None);
        let h2 = compute_input_hash("sys", "usr-b", None);
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_differs_with_vs_without_script_output() {
        let h1 = compute_input_hash("sys", "usr", None);
        let h2 = compute_input_hash("sys", "usr", Some("out"));
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_is_64_hex_chars() {
        let h = compute_input_hash("a", "b", None);
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_nul_separation_prevents_boundary_collision() {
        // "sys" + "usr" should differ from "sy" + "susr" (shifted boundary).
        let h1 = compute_input_hash("sys", "usr", None);
        let h2 = compute_input_hash("sy", "susr", None);
        assert_ne!(h1, h2);
    }

    // ── extract_summary ───────────────────────────────────────────────────

    #[test]
    fn extract_summary_first_paragraph() {
        assert_eq!(extract_summary("first para\n\nsecond para"), "first para");
    }

    #[test]
    fn extract_summary_empty_body() {
        assert_eq!(extract_summary(""), "");
    }

    #[test]
    fn extract_summary_silent_only() {
        assert_eq!(extract_summary("[SILENT]"), "");
    }

    #[test]
    fn extract_summary_truncates_long_body() {
        let long = "word ".repeat(60); // 300 chars
        let result = extract_summary(&long);
        assert!(result.chars().count() <= 282); // 280 + "…"
        assert!(result.ends_with('…'));
    }

    #[test]
    fn extract_summary_no_truncation_under_limit() {
        let short = "short text";
        assert_eq!(extract_summary(short), "short text");
    }

    // ── wrap_for_injection ────────────────────────────────────────────────

    #[test]
    fn wrap_adds_delimiters() {
        let wrapped = wrap_for_injection("body text", "my-routine");
        assert!(wrapped.contains(PACKET_OPEN));
        assert!(wrapped.contains(PACKET_CLOSE));
        assert!(wrapped.contains("name=my-routine"));
        assert!(wrapped.contains("body text"));
    }

    #[test]
    fn wrap_strips_pre_existing_open_delimiter() {
        let body = format!("legit text {}injected", PACKET_OPEN);
        let wrapped = wrap_for_injection(&body, "rtn");
        // The pre-existing delimiter must not appear inside the wrapped body.
        // Count occurrences: should be exactly 1 (the outer wrapper itself).
        let count = wrapped.matches(PACKET_OPEN).count();
        assert_eq!(count, 1, "pre-existing PACKET_OPEN should be stripped");
    }

    #[test]
    fn wrap_strips_pre_existing_close_delimiter() {
        let body = format!("{}injected close", PACKET_CLOSE);
        let wrapped = wrap_for_injection(&body, "rtn");
        let count = wrapped.matches(PACKET_CLOSE).count();
        assert_eq!(count, 1, "pre-existing PACKET_CLOSE should be stripped");
    }

    // ── RoutinePacket serde round-trip ────────────────────────────────────

    #[test]
    fn packet_serde_roundtrip() {
        let p = sample_packet();
        let json = serde_json::to_string(&p).unwrap();
        let back: RoutinePacket = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn packet_status_serde_all_variants() {
        for status in [
            PacketStatus::Clean,
            PacketStatus::Silent,
            PacketStatus::Failed,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let back: PacketStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
    }

    // ── write_packet path derivation ──────────────────────────────────────

    #[test]
    fn packet_path_has_json_extension() {
        let path = packet_path_for("rtn-abc12345", 946_684_800).unwrap();
        assert!(path.to_string_lossy().ends_with(".json"));
        assert!(path.to_string_lossy().contains("rtn-abc12345"));
        assert!(path.to_string_lossy().contains("20000101T000000Z"));
    }

    #[test]
    fn packet_path_rejects_bad_routine_id() {
        assert!(packet_path_for("../bad", 0).is_err());
    }
}
