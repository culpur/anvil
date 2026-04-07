//! Credential Vault — AES-256-GCM envelope encryption with Argon2id key derivation.
//!
//! Storage layout: `~/.anvil/vault/`
//!   - `vault.meta`         — salt + KDF params (JSON, plaintext)
//!   - `cred_<label>.enc`   — encrypted credential (JSON, base64 ciphertext)
//!   - `totp_<label>.enc`   — encrypted TOTP entry (JSON, base64 ciphertext)
//!
//! Encryption model:
//!   Master password → Argon2id → KEK (32 bytes)
//!   Per-credential random DEK (32 bytes) → AES-256-GCM encrypt(plaintext)
//!   DEK itself is AES-256-GCM encrypted with the KEK and stored alongside the ciphertext.
//!
//! TOTP: HMAC-SHA1, Base32 secret, 6-digit code, 30-second window (RFC 6238).

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::{
    password_hash::SaltString,
    Argon2, Params,
};
use base32::Alphabet;
use hmac::Hmac;
use rand::RngCore;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha1::Sha1;

// ─── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum VaultError {
    Io(std::io::Error),
    Serialization(String),
    Crypto(String),
    Locked,
    NotFound(String),
    AlreadyExists(String),
    InvalidMasterPassword,
    NotInitialized,
    InvalidTotpUri(String),
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Serialization(s) => write!(f, "Serialization error: {s}"),
            Self::Crypto(s) => write!(f, "Crypto error: {s}"),
            Self::Locked => write!(f, "Vault is locked — run /vault unlock first"),
            Self::NotFound(label) => write!(f, "Entry not found: {label}"),
            Self::AlreadyExists(label) => write!(f, "Entry already exists: {label}"),
            Self::InvalidMasterPassword => write!(f, "Invalid master password"),
            Self::NotInitialized => write!(f, "Vault not initialized — run /vault setup first"),
            Self::InvalidTotpUri(s) => write!(f, "Invalid TOTP URI: {s}"),
        }
    }
}

impl std::error::Error for VaultError {}

impl From<std::io::Error> for VaultError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ─── On-disk structures ───────────────────────────────────────────────────────

/// Vault metadata stored in vault.meta (plaintext JSON).
#[derive(Debug, Serialize, Deserialize)]
struct VaultMeta {
    /// Argon2id salt (base64-encoded).
    salt: String,
    /// Argon2id memory cost in KiB.
    m_cost: u32,
    /// Argon2id iteration count.
    t_cost: u32,
    /// Argon2id parallelism.
    p_cost: u32,
    /// Verification token: AES-256-GCM encrypt("anvil-vault-v1") with KEK.
    /// Used to verify the master password on unlock without storing the KEK.
    verify_nonce: String,
    verify_ciphertext: String,
}

/// An encrypted envelope storing one value.
#[derive(Debug, Serialize, Deserialize)]
struct EncryptedEnvelope {
    /// Label for human reference.
    label: String,
    /// DEK nonce (base64).
    dek_nonce: String,
    /// DEK ciphertext encrypted under KEK (base64).
    dek_ciphertext: String,
    /// Data nonce (base64).
    data_nonce: String,
    /// Data ciphertext encrypted under DEK (base64).
    data_ciphertext: String,
}

/// Plaintext credential.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Credential {
    pub label: String,
    pub username: Option<String>,
    pub secret: String,
    pub notes: Option<String>,
    pub created_at: u64,
}

/// Plaintext TOTP entry.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TotpEntry {
    pub label: String,
    /// Base32-encoded TOTP secret.
    pub secret: String,
    /// Issuer (from otpauth URI, optional).
    pub issuer: Option<String>,
    /// Account name.
    pub account: Option<String>,
    pub created_at: u64,
}

// ─── VaultManager ─────────────────────────────────────────────────────────────

/// Manages encrypted credential storage and TOTP generation.
///
/// Call [`VaultManager::setup`] once to initialise, then [`VaultManager::unlock`]
/// each session.  All credential/TOTP operations require the vault to be unlocked.
pub struct VaultManager {
    vault_dir: PathBuf,
    /// In-memory KEK, present only when unlocked.
    kek: Option<[u8; 32]>,
}

impl VaultManager {
    /// Return the default vault directory (`~/.anvil/vault/`).
    #[must_use]
    pub fn default_vault_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".anvil").join("vault")
    }

    /// Create a new manager pointing at `vault_dir`.
    #[must_use]
    pub fn new(vault_dir: PathBuf) -> Self {
        Self {
            vault_dir,
            kek: None,
        }
    }

    /// Create a manager using the default path.
    #[must_use]
    pub fn with_default_dir() -> Self {
        Self::new(Self::default_vault_dir())
    }

    // ─── Setup & lifecycle ────────────────────────────────────────────────────

    /// Returns true if the vault has been initialised (vault.meta exists).
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.vault_dir.join("vault.meta").exists()
    }

    /// Returns true if the vault is currently unlocked (KEK held in memory).
    #[must_use]
    pub fn is_unlocked(&self) -> bool {
        self.kek.is_some()
    }

    /// Initialise the vault with a master password.  Fails if already initialised.
    pub fn setup(&mut self, master_password: &str) -> Result<(), VaultError> {
        if self.is_initialized() {
            return Err(VaultError::AlreadyExists("vault".into()));
        }
        fs::create_dir_all(&self.vault_dir)?;

        // Generate Argon2id salt.
        let salt = SaltString::generate(&mut OsRng);

        let params = Params::new(65536, 3, 4, Some(32))
            .map_err(|e| VaultError::Crypto(e.to_string()))?;
        let kek = derive_key(master_password, salt.as_str(), &params)?;

        // Produce verification token.
        let (verify_nonce, verify_ciphertext) =
            aes_encrypt(&kek, b"anvil-vault-v1")?;

        let meta = VaultMeta {
            salt: salt.to_string(),
            m_cost: 65536,
            t_cost: 3,
            p_cost: 4,
            verify_nonce: base64_encode(&verify_nonce),
            verify_ciphertext: base64_encode(&verify_ciphertext),
        };
        let meta_path = self.vault_dir.join("vault.meta");
        let meta_json = serde_json::to_string_pretty(&meta)
            .map_err(|e| VaultError::Serialization(e.to_string()))?;
        write_secret_file(&meta_path, meta_json.as_bytes())?;

        self.kek = Some(kek);
        Ok(())
    }

    /// Unlock the vault with the master password, deriving the KEK into memory.
    pub fn unlock(&mut self, master_password: &str) -> Result<(), VaultError> {
        let meta = self.load_meta()?;
        let params = Params::new(meta.m_cost, meta.t_cost, meta.p_cost, Some(32))
            .map_err(|e| VaultError::Crypto(e.to_string()))?;
        let kek = derive_key(master_password, &meta.salt, &params)?;

        // Verify using the stored token.
        let nonce_bytes = base64_decode(&meta.verify_nonce)?;
        let ct_bytes = base64_decode(&meta.verify_ciphertext)?;
        let plaintext = aes_decrypt(&kek, &nonce_bytes, &ct_bytes)
            .map_err(|_| VaultError::InvalidMasterPassword)?;

        if plaintext != b"anvil-vault-v1" {
            return Err(VaultError::InvalidMasterPassword);
        }
        self.kek = Some(kek);
        Ok(())
    }

    /// Lock the vault — zeroes and drops the in-memory KEK.
    pub fn lock(&mut self) {
        if let Some(mut kek) = self.kek.take() {
            kek.iter_mut().for_each(|b| *b = 0);
        }
    }

    // ─── Credentials ──────────────────────────────────────────────────────────

    /// Store an encrypted credential.  Fails if the label already exists.
    pub fn store_credential(&self, cred: &Credential) -> Result<(), VaultError> {
        let kek = self.require_kek()?;
        let path = self.cred_path(&cred.label);
        if path.exists() {
            return Err(VaultError::AlreadyExists(cred.label.clone()));
        }
        let plaintext = serde_json::to_vec(cred)
            .map_err(|e| VaultError::Serialization(e.to_string()))?;
        let envelope = build_envelope(kek, &cred.label, &plaintext)?;
        write_envelope(&path, &envelope)
    }

    /// Decrypt and return a credential by label.
    pub fn get_credential(&self, label: &str) -> Result<Credential, VaultError> {
        let kek = self.require_kek()?;
        let path = self.cred_path(label);
        if !path.exists() {
            return Err(VaultError::NotFound(label.into()));
        }
        let plaintext = open_envelope(kek, &path)?;
        serde_json::from_slice(&plaintext)
            .map_err(|e| VaultError::Serialization(e.to_string()))
    }

    /// Overwrite an existing credential.
    pub fn update_credential(&self, cred: &Credential) -> Result<(), VaultError> {
        let kek = self.require_kek()?;
        let path = self.cred_path(&cred.label);
        if !path.exists() {
            return Err(VaultError::NotFound(cred.label.clone()));
        }
        let plaintext = serde_json::to_vec(cred)
            .map_err(|e| VaultError::Serialization(e.to_string()))?;
        let envelope = build_envelope(kek, &cred.label, &plaintext)?;
        write_envelope(&path, &envelope)
    }

    /// List labels of all stored credentials.
    pub fn list_credentials(&self) -> Result<Vec<String>, VaultError> {
        self.require_kek()?;
        list_entries(&self.vault_dir, "cred_", ".enc")
    }

    /// Delete a credential by label.
    pub fn delete_credential(&self, label: &str) -> Result<(), VaultError> {
        self.require_kek()?;
        let path = self.cred_path(label);
        if !path.exists() {
            return Err(VaultError::NotFound(label.into()));
        }
        fs::remove_file(path)?;
        Ok(())
    }

    // ─── TOTP ─────────────────────────────────────────────────────────────────

    /// Add a TOTP entry.  Fails if the label already exists.
    pub fn add_totp(&self, entry: &TotpEntry) -> Result<(), VaultError> {
        let kek = self.require_kek()?;
        let path = self.totp_path(&entry.label);
        if path.exists() {
            return Err(VaultError::AlreadyExists(entry.label.clone()));
        }
        let plaintext = serde_json::to_vec(entry)
            .map_err(|e| VaultError::Serialization(e.to_string()))?;
        let envelope = build_envelope(kek, &entry.label, &plaintext)?;
        write_envelope(&path, &envelope)
    }

    /// Generate the current 6-digit TOTP code for a label.
    pub fn generate_totp(&self, label: &str) -> Result<TotpCode, VaultError> {
        let entry = self.get_totp_entry(label)?;
        let secret_bytes = base32::decode(Alphabet::Rfc4648 { padding: false }, &entry.secret)
            .or_else(|| base32::decode(Alphabet::Rfc4648 { padding: true }, &entry.secret))
            .ok_or_else(|| VaultError::Crypto("Invalid Base32 TOTP secret".into()))?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| VaultError::Crypto(e.to_string()))?
            .as_secs();
        let counter = now / 30;
        let remaining = 30 - (now % 30);
        let code = hotp(&secret_bytes, counter)?;
        Ok(TotpCode {
            code: format!("{code:06}"),
            remaining_secs: remaining,
        })
    }

    /// List labels of all stored TOTP entries.
    pub fn list_totp(&self) -> Result<Vec<String>, VaultError> {
        self.require_kek()?;
        list_entries(&self.vault_dir, "totp_", ".enc")
    }

    /// Import a TOTP entry from an `otpauth://` URI.
    ///
    /// Format: `otpauth://totp/<issuer>:<account>?secret=BASE32&issuer=<issuer>`
    pub fn import_totp_uri(&self, label: &str, uri: &str) -> Result<(), VaultError> {
        let entry = parse_otpauth_uri(label, uri)?;
        self.add_totp(&entry)
    }

    /// Delete a TOTP entry by label.
    pub fn delete_totp(&self, label: &str) -> Result<(), VaultError> {
        self.require_kek()?;
        let path = self.totp_path(label);
        if !path.exists() {
            return Err(VaultError::NotFound(label.into()));
        }
        fs::remove_file(path)?;
        Ok(())
    }

    // ─── Internal helpers ─────────────────────────────────────────────────────

    fn require_kek(&self) -> Result<&[u8; 32], VaultError> {
        self.kek.as_ref().ok_or(VaultError::Locked)
    }

    fn load_meta(&self) -> Result<VaultMeta, VaultError> {
        let meta_path = self.vault_dir.join("vault.meta");
        if !meta_path.exists() {
            return Err(VaultError::NotInitialized);
        }
        let raw = fs::read_to_string(&meta_path)?;
        serde_json::from_str(&raw).map_err(|e| VaultError::Serialization(e.to_string()))
    }

    fn cred_path(&self, label: &str) -> PathBuf {
        self.vault_dir.join(format!("cred_{}.enc", sanitize_label(label)))
    }

    fn totp_path(&self, label: &str) -> PathBuf {
        self.vault_dir.join(format!("totp_{}.enc", sanitize_label(label)))
    }

    fn get_totp_entry(&self, label: &str) -> Result<TotpEntry, VaultError> {
        let kek = self.require_kek()?;
        let path = self.totp_path(label);
        if !path.exists() {
            return Err(VaultError::NotFound(label.into()));
        }
        let plaintext = open_envelope(kek, &path)?;
        serde_json::from_slice(&plaintext)
            .map_err(|e| VaultError::Serialization(e.to_string()))
    }
}

impl Drop for VaultManager {
    fn drop(&mut self) {
        self.lock();
    }
}

// ─── TOTP output ──────────────────────────────────────────────────────────────

/// A generated TOTP code with its remaining validity window.
#[derive(Debug, Clone)]
pub struct TotpCode {
    /// Six-digit code, zero-padded.
    pub code: String,
    /// Seconds until this code expires.
    pub remaining_secs: u64,
}

// ─── Crypto helpers ───────────────────────────────────────────────────────────

/// Derive a 32-byte KEK from a password + salt using Argon2id.
fn derive_key(password: &str, salt_str: &str, params: &Params) -> Result<[u8; 32], VaultError> {
    use argon2::Algorithm;
    use argon2::Version;

    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params.clone());
    let mut key = [0u8; 32];
    argon2
        .hash_password_into(password.as_bytes(), salt_str.as_bytes(), &mut key)
        .map_err(|e| VaultError::Crypto(e.to_string()))?;
    Ok(key)
}

/// Encrypt `plaintext` with AES-256-GCM using the given key.
/// Returns `(nonce_bytes, ciphertext_bytes)`.
fn aes_encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), VaultError> {
    let cipher_key = Key::<Aes256Gcm>::from_slice(key);
    let cipher = Aes256Gcm::new(cipher_key);
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| VaultError::Crypto(e.to_string()))?;
    Ok((nonce_bytes.to_vec(), ciphertext))
}

/// Decrypt `ciphertext` with AES-256-GCM.
fn aes_decrypt(key: &[u8; 32], nonce_bytes: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, VaultError> {
    let cipher_key = Key::<Aes256Gcm>::from_slice(key);
    let cipher = Aes256Gcm::new(cipher_key);
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| VaultError::Crypto(e.to_string()))
}

/// Build an `EncryptedEnvelope` using envelope encryption:
/// 1. Generate a random 32-byte DEK.
/// 2. Encrypt `plaintext` with the DEK.
/// 3. Encrypt the DEK with the KEK.
fn build_envelope(kek: &[u8; 32], label: &str, plaintext: &[u8]) -> Result<EncryptedEnvelope, VaultError> {
    // Generate DEK.
    let mut dek = [0u8; 32];
    OsRng.fill_bytes(&mut dek);

    // Encrypt data with DEK.
    let (data_nonce, data_ct) = aes_encrypt(&dek, plaintext)?;

    // Encrypt DEK with KEK.
    let (dek_nonce, dek_ct) = aes_encrypt(kek, &dek)?;

    // Zero DEK from stack.
    dek.iter_mut().for_each(|b| *b = 0);

    Ok(EncryptedEnvelope {
        label: label.to_string(),
        dek_nonce: base64_encode(&dek_nonce),
        dek_ciphertext: base64_encode(&dek_ct),
        data_nonce: base64_encode(&data_nonce),
        data_ciphertext: base64_encode(&data_ct),
    })
}

/// Decrypt an `EncryptedEnvelope`, returning the plaintext bytes.
fn open_envelope(kek: &[u8; 32], path: &Path) -> Result<Vec<u8>, VaultError> {
    let raw = fs::read_to_string(path)?;
    let envelope: EncryptedEnvelope = serde_json::from_str(&raw)
        .map_err(|e| VaultError::Serialization(e.to_string()))?;

    let dek_nonce = base64_decode(&envelope.dek_nonce)?;
    let dek_ct = base64_decode(&envelope.dek_ciphertext)?;
    let dek_bytes = aes_decrypt(kek, &dek_nonce, &dek_ct)
        .map_err(|_| VaultError::InvalidMasterPassword)?;

    if dek_bytes.len() != 32 {
        return Err(VaultError::Crypto("DEK length mismatch".into()));
    }
    let mut dek = [0u8; 32];
    dek.copy_from_slice(&dek_bytes);

    let data_nonce = base64_decode(&envelope.data_nonce)?;
    let data_ct = base64_decode(&envelope.data_ciphertext)?;
    let plaintext = aes_decrypt(&dek, &data_nonce, &data_ct)?;

    dek.iter_mut().for_each(|b| *b = 0);
    Ok(plaintext)
}

fn write_envelope(path: &Path, envelope: &EncryptedEnvelope) -> Result<(), VaultError> {
    let json = serde_json::to_string_pretty(envelope)
        .map_err(|e| VaultError::Serialization(e.to_string()))?;
    write_secret_file(path, json.as_bytes())
}

/// Write `data` to `path` with mode 0o600 (owner read/write only).
/// Creates or truncates the file.
fn write_secret_file(path: &Path, data: &[u8]) -> Result<(), VaultError> {
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

// ─── HOTP / TOTP ─────────────────────────────────────────────────────────────

/// HOTP (RFC 4226) — compute a 6-digit code for a given counter.
fn hotp(secret: &[u8], counter: u64) -> Result<u32, VaultError> {
    use hmac::Mac as _;
    type HmacSha1 = Hmac<Sha1>;
    let mut mac = <HmacSha1 as hmac::Mac>::new_from_slice(secret)
        .map_err(|e: hmac::digest::InvalidLength| VaultError::Crypto(e.to_string()))?;
    mac.update(&counter.to_be_bytes());
    let result = mac.finalize().into_bytes();

    // Dynamic truncation (RFC 4226 §5.3).
    let offset = (result[19] & 0x0f) as usize;
    let code = u32::from_be_bytes([
        result[offset] & 0x7f,
        result[offset + 1],
        result[offset + 2],
        result[offset + 3],
    ]);
    Ok(code % 1_000_000)
}

// ─── TOTP URI parsing ─────────────────────────────────────────────────────────

/// Parse an `otpauth://totp/...` URI into a `TotpEntry`.
fn parse_otpauth_uri(label: &str, uri: &str) -> Result<TotpEntry, VaultError> {
    if !uri.starts_with("otpauth://totp/") {
        return Err(VaultError::InvalidTotpUri(
            "URI must begin with otpauth://totp/".into(),
        ));
    }

    // Split path and query string.
    let after_scheme = uri.trim_start_matches("otpauth://totp/");
    let (path_part, query_part) = after_scheme
        .split_once('?')
        .unwrap_or((after_scheme, ""));

    // Decode the path (issuer:account or just account).
    let decoded_path = url_decode(path_part);
    let (issuer_from_path, account) = if let Some(colon) = decoded_path.find(':') {
        let iss = decoded_path[..colon].to_string();
        let acc = decoded_path[colon + 1..].to_string();
        (Some(iss), Some(acc))
    } else {
        (None, Some(decoded_path))
    };

    // Parse query parameters.
    let mut params: HashMap<String, String> = HashMap::new();
    for kv in query_part.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            params.insert(k.to_ascii_lowercase(), url_decode(v));
        }
    }

    let secret = params
        .get("secret")
        .ok_or_else(|| VaultError::InvalidTotpUri("Missing 'secret' parameter".into()))?
        .to_ascii_uppercase();

    let issuer = params.get("issuer").cloned().or(issuer_from_path);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    Ok(TotpEntry {
        label: label.to_string(),
        secret,
        issuer,
        account,
        created_at: now,
    })
}

/// Minimal percent-decoding for URI path/query segments.
///
/// Collects decoded bytes into a Vec<u8> first, then converts to String so that
/// multi-byte UTF-8 sequences (e.g. %C3%A9 → é) are reassembled correctly rather
/// than being cast byte-by-byte through `char`, which would produce garbage.
fn url_decode(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                bytes.push(byte);
            }
        } else if c == '+' {
            bytes.push(b' ');
        } else {
            let mut buf = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| s.to_string())
}

// ─── Misc helpers ─────────────────────────────────────────────────────────────

fn base64_encode(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    STANDARD.encode(bytes)
}

fn base64_decode(s: &str) -> Result<Vec<u8>, VaultError> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    STANDARD
        .decode(s.trim())
        .map_err(|e| VaultError::Crypto(format!("base64 decode: {e}")))
}

/// Strip characters that are unsafe in filenames.
fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .collect()
}

/// List entry labels from files matching `prefix_<label>.enc` in `dir`.
fn list_entries(dir: &Path, prefix: &str, suffix: &str) -> Result<Vec<String>, VaultError> {
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_vault() -> (VaultManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let vm = VaultManager::new(dir.path().join("vault"));
        (vm, dir)
    }

    #[test]
    fn setup_and_unlock() {
        let (mut vm, _dir) = temp_vault();
        vm.setup("hunter2").unwrap();
        assert!(vm.is_initialized());
        assert!(vm.is_unlocked());
        vm.lock();
        assert!(!vm.is_unlocked());
        vm.unlock("hunter2").unwrap();
        assert!(vm.is_unlocked());
    }

    #[test]
    fn wrong_password_rejected() {
        let (mut vm, _dir) = temp_vault();
        vm.setup("correct").unwrap();
        vm.lock();
        assert!(vm.unlock("wrong").is_err());
    }

    #[test]
    fn credential_roundtrip() {
        let (mut vm, _dir) = temp_vault();
        vm.setup("pw123").unwrap();
        let cred = Credential {
            label: "github".into(),
            username: Some("alice".into()),
            secret: "ghp_supersecret".into(),
            notes: Some("personal".into()),
            created_at: 0,
        };
        vm.store_credential(&cred).unwrap();
        let got = vm.get_credential("github").unwrap();
        assert_eq!(got.secret, "ghp_supersecret");
        assert_eq!(got.username.as_deref(), Some("alice"));
    }

    #[test]
    fn list_and_delete_credentials() {
        let (mut vm, _dir) = temp_vault();
        vm.setup("pw123").unwrap();
        for label in ["alpha", "beta", "gamma"] {
            vm.store_credential(&Credential {
                label: label.into(),
                username: None,
                secret: label.into(),
                notes: None,
                created_at: 0,
            })
            .unwrap();
        }
        let mut list = vm.list_credentials().unwrap();
        list.sort();
        assert_eq!(list, vec!["alpha", "beta", "gamma"]);
        vm.delete_credential("beta").unwrap();
        let mut list2 = vm.list_credentials().unwrap();
        list2.sort();
        assert_eq!(list2, vec!["alpha", "gamma"]);
    }

    #[test]
    fn totp_known_vector() {
        // RFC 6238 test vector: secret = "12345678901234567890", T=59 → counter=1, expected=287082
        let secret = b"12345678901234567890";
        let code = hotp(secret, 1).unwrap();
        assert_eq!(code, 287_082);
    }

    #[test]
    fn totp_generate_live() {
        let (mut vm, _dir) = temp_vault();
        vm.setup("pw").unwrap();
        let entry = TotpEntry {
            label: "myapp".into(),
            secret: "JBSWY3DPEHPK3PXP".into(), // "Hello!" in base32
            issuer: Some("MyApp".into()),
            account: Some("alice@example.com".into()),
            created_at: 0,
        };
        vm.add_totp(&entry).unwrap();
        let code = vm.generate_totp("myapp").unwrap();
        assert_eq!(code.code.len(), 6);
        assert!(code.remaining_secs <= 30);
    }

    #[test]
    fn parse_otpauth_uri_roundtrip() {
        let uri = "otpauth://totp/Example%3Aalice%40example.com?secret=JBSWY3DPEHPK3PXP&issuer=Example";
        let entry = parse_otpauth_uri("example", uri).unwrap();
        assert_eq!(entry.secret, "JBSWY3DPEHPK3PXP");
        assert_eq!(entry.issuer.as_deref(), Some("Example"));
    }

    #[test]
    fn base64_roundtrip() {
        let original = b"Hello, AES-256-GCM vault!";
        let encoded = base64_encode(original);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }
}
