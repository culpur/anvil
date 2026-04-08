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

pub mod crypto;
pub mod storage;

use std::fs;
use std::path::PathBuf;

use argon2::{password_hash::SaltString, Params};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

use crypto::{
    aes_decrypt, aes_encrypt, base64_decode, base64_encode, build_envelope,
    derive_key, generate_totp_code, parse_otpauth_uri,
};
use storage::{
    cred_path, list_entries, load_meta, open_envelope, totp_path, write_envelope, write_meta,
    VaultMeta,
};

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

// ─── Public data types ────────────────────────────────────────────────────────

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

/// A generated TOTP code with its remaining validity window.
#[derive(Debug, Clone)]
pub struct TotpCode {
    /// Six-digit code, zero-padded.
    pub code: String,
    /// Seconds until this code expires.
    pub remaining_secs: u64,
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
        let (verify_nonce, verify_ciphertext) = aes_encrypt(&kek, b"anvil-vault-v1")?;

        let meta = VaultMeta {
            salt: salt.to_string(),
            m_cost: 65536,
            t_cost: 3,
            p_cost: 4,
            verify_nonce: base64_encode(&verify_nonce),
            verify_ciphertext: base64_encode(&verify_ciphertext),
        };
        write_meta(&self.vault_dir, &meta)?;

        self.kek = Some(kek);
        Ok(())
    }

    /// Unlock the vault with the master password, deriving the KEK into memory.
    pub fn unlock(&mut self, master_password: &str) -> Result<(), VaultError> {
        let meta = load_meta(&self.vault_dir)?;
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
            for b in kek.iter_mut() { *b = 0; }
        }
    }

    // ─── Credentials ──────────────────────────────────────────────────────────

    /// Store an encrypted credential.  Fails if the label already exists.
    pub fn store_credential(&self, cred: &Credential) -> Result<(), VaultError> {
        let kek = self.require_kek()?;
        let path = cred_path(&self.vault_dir, &cred.label);
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
        let path = cred_path(&self.vault_dir, label);
        if !path.exists() {
            return Err(VaultError::NotFound(label.into()));
        }
        let plaintext = open_envelope(kek, &path)?;
        serde_json::from_slice(&plaintext)
            .map_err(|e| VaultError::Serialization(e.to_string()))
    }

    /// Store a credential, overwriting it if a credential with the same label
    /// already exists.  This is the upsert variant used during setup/migration.
    pub fn upsert_credential(&self, cred: &Credential) -> Result<(), VaultError> {
        let kek = self.require_kek()?;
        let path = cred_path(&self.vault_dir, &cred.label);
        let plaintext = serde_json::to_vec(cred)
            .map_err(|e| VaultError::Serialization(e.to_string()))?;
        let envelope = build_envelope(kek, &cred.label, &plaintext)?;
        write_envelope(&path, &envelope)
    }

    /// Overwrite an existing credential.
    pub fn update_credential(&self, cred: &Credential) -> Result<(), VaultError> {
        let kek = self.require_kek()?;
        let path = cred_path(&self.vault_dir, &cred.label);
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
        let path = cred_path(&self.vault_dir, label);
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
        let path = totp_path(&self.vault_dir, &entry.label);
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
        let (code, remaining_secs) = generate_totp_code(&entry)?;
        Ok(TotpCode { code, remaining_secs })
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
        let path = totp_path(&self.vault_dir, label);
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

    fn get_totp_entry(&self, label: &str) -> Result<TotpEntry, VaultError> {
        let kek = self.require_kek()?;
        let path = totp_path(&self.vault_dir, label);
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::crypto::hotp;

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
        use crate::vault::crypto::parse_otpauth_uri;
        let uri = "otpauth://totp/Example%3Aalice%40example.com?secret=JBSWY3DPEHPK3PXP&issuer=Example";
        let entry = parse_otpauth_uri("example", uri).unwrap();
        assert_eq!(entry.secret, "JBSWY3DPEHPK3PXP");
        assert_eq!(entry.issuer.as_deref(), Some("Example"));
    }

    #[test]
    fn base64_roundtrip() {
        use crate::vault::crypto::{base64_decode, base64_encode};
        let original = b"Hello, AES-256-GCM vault!";
        let encoded = base64_encode(original);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }
}
