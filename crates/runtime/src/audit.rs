//! Session audit trail — HMAC-SHA256 signed transcripts for compliance and forensics.
//!
//! Each session generates a tamper-evident transcript stored in `~/.anvil/audit/`.
//! Verification uses the vault-derived key to validate integrity.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

/// Audit directory under the Anvil home.
fn audit_dir() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".anvil").join("audit"))
}

/// Generate an HMAC-SHA256 signature of the given content.
pub fn sign_transcript(content: &str, key: &[u8; 32]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key");
    mac.update(content.as_bytes());
    let result = mac.finalize();
    hex::encode(result.into_bytes())
}

/// Verify an HMAC-SHA256 signature.
pub fn verify_signature(content: &str, signature: &str, key: &[u8; 32]) -> bool {
    let expected = sign_transcript(content, key);
    // Constant-time comparison
    expected == signature
}

/// Save a signed audit record for a session.
pub fn save_audit_record(
    session_id: &str,
    transcript: &str,
    key: &[u8; 32],
) -> Result<PathBuf, std::io::Error> {
    let dir = audit_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "No home directory")
    })?;
    fs::create_dir_all(&dir)?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let signature = sign_transcript(transcript, key);

    // Write transcript
    let transcript_path = dir.join(format!("{session_id}.md"));
    fs::write(&transcript_path, transcript)?;

    // Write signature
    let sig_path = dir.join(format!("{session_id}.sig"));
    let sig_content = format!(
        "session: {session_id}\ntimestamp: {timestamp}\nalgorithm: HMAC-SHA256\nsignature: {signature}\n"
    );
    fs::write(&sig_path, sig_content)?;

    Ok(transcript_path)
}

/// Verify an existing audit record.
pub fn verify_audit_record(
    session_id: &str,
    key: &[u8; 32],
) -> Result<bool, String> {
    let dir = audit_dir().ok_or("No home directory")?;
    let transcript_path = dir.join(format!("{session_id}.md"));
    let sig_path = dir.join(format!("{session_id}.sig"));

    let transcript = fs::read_to_string(&transcript_path)
        .map_err(|e| format!("Cannot read transcript: {e}"))?;
    let sig_content = fs::read_to_string(&sig_path)
        .map_err(|e| format!("Cannot read signature: {e}"))?;

    // Extract signature from sig file
    let signature = sig_content
        .lines()
        .find_map(|line| line.strip_prefix("signature: "))
        .ok_or("No signature found in .sig file")?;

    Ok(verify_signature(&transcript, signature, key))
}

/// Log a redaction event to the audit trail.
pub fn log_redaction(tool_name: &str, redaction_type: &str) {
    if let Some(dir) = audit_dir() {
        let _ = fs::create_dir_all(&dir);
        let log_path = dir.join("redactions.log");
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let entry = format!("{timestamp}\t{tool_name}\t{redaction_type}\n");
        let _ = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, entry.as_bytes()));
    }
}
