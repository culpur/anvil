//! Filesystem operations: vault file layout, serialization, and secure file writing.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::VaultError;
use super::crypto::open_envelope_data;

// ─── On-disk structures ───────────────────────────────────────────────────────

/// Vault metadata stored in vault.meta (plaintext JSON).
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct VaultMeta {
    /// Argon2id salt (base64-encoded).
    pub(super) salt: String,
    /// Argon2id memory cost in KiB.
    pub(super) m_cost: u32,
    /// Argon2id iteration count.
    pub(super) t_cost: u32,
    /// Argon2id parallelism.
    pub(super) p_cost: u32,
    /// Verification token: AES-256-GCM encrypt("anvil-vault-v1") with KEK.
    /// Used to verify the master password on unlock without storing the KEK.
    pub(super) verify_nonce: String,
    pub(super) verify_ciphertext: String,
}

/// An encrypted envelope storing one value.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct EncryptedEnvelope {
    /// Label for human reference.
    pub(super) label: String,
    /// DEK nonce (base64).
    pub(super) dek_nonce: String,
    /// DEK ciphertext encrypted under KEK (base64).
    pub(super) dek_ciphertext: String,
    /// Data nonce (base64).
    pub(super) data_nonce: String,
    /// Data ciphertext encrypted under DEK (base64).
    pub(super) data_ciphertext: String,
}

// ─── Filesystem helpers ───────────────────────────────────────────────────────

/// Read and deserialize vault metadata from disk.
pub(super) fn load_meta(vault_dir: &Path) -> Result<VaultMeta, VaultError> {
    let meta_path = vault_dir.join("vault.meta");
    if !meta_path.exists() {
        return Err(VaultError::NotInitialized);
    }
    let raw = fs::read_to_string(&meta_path)?;
    serde_json::from_str(&raw).map_err(|e| VaultError::Serialization(e.to_string()))
}

/// Write vault metadata to disk.
pub(super) fn write_meta(vault_dir: &Path, meta: &VaultMeta) -> Result<(), VaultError> {
    let meta_path = vault_dir.join("vault.meta");
    let meta_json = serde_json::to_string_pretty(meta)
        .map_err(|e| VaultError::Serialization(e.to_string()))?;
    write_secret_file(&meta_path, meta_json.as_bytes())
}

/// Decrypt and return the plaintext bytes from an envelope file at `path`.
pub(super) fn open_envelope(kek: &[u8; 32], path: &Path) -> Result<Vec<u8>, VaultError> {
    let raw = fs::read_to_string(path)?;
    let envelope: EncryptedEnvelope = serde_json::from_str(&raw)
        .map_err(|e| VaultError::Serialization(e.to_string()))?;
    open_envelope_data(kek, &envelope)
}

/// Serialize and write an `EncryptedEnvelope` to `path`.
pub(super) fn write_envelope(path: &Path, envelope: &EncryptedEnvelope) -> Result<(), VaultError> {
    let json = serde_json::to_string_pretty(envelope)
        .map_err(|e| VaultError::Serialization(e.to_string()))?;
    write_secret_file(path, json.as_bytes())
}

/// Write `data` to `path` with mode 0o600 (owner read/write only).
/// Creates or truncates the file.
pub(super) fn write_secret_file(path: &Path, data: &[u8]) -> Result<(), VaultError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(data)?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, data)?;
    }
    Ok(())
}

/// Return the file path for a credential entry.
pub(super) fn cred_path(vault_dir: &Path, label: &str) -> PathBuf {
    vault_dir.join(format!("cred_{}.enc", sanitize_label(label)))
}

/// Return the file path for a TOTP entry.
pub(super) fn totp_path(vault_dir: &Path, label: &str) -> PathBuf {
    vault_dir.join(format!("totp_{}.enc", sanitize_label(label)))
}

/// Strip characters that are unsafe in filenames.
pub(super) fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// List entry labels from files matching `prefix_<label>.enc` in `dir`.
pub(super) fn list_entries(
    dir: &Path,
    prefix: &str,
    suffix: &str,
) -> Result<Vec<String>, VaultError> {
    let mut labels = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(prefix) && name_str.ends_with(suffix) {
            let label = name_str[prefix.len()..name_str.len() - suffix.len()].to_string();
            labels.push(label);
        }
    }
    labels.sort();
    Ok(labels)
}
