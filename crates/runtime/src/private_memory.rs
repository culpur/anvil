//! Encrypted private project memory — sensitive infrastructure facts (hostnames,
//! IPs, deploy paths, port numbers) that are too sensitive for plaintext ANVIL.md
//! but are not credentials that belong in the credential vault.
//!
//! Storage layout: `~/.anvil/private/{project-hash}.enc`
//!
//! Each project gets its own encrypted file keyed by the SHA-256 of its absolute
//! root path.  The file is a single AES-256-GCM blob: a 12-byte random nonce
//! followed by the ciphertext of a UTF-8 JSON object (`{"key": "value", ...}`).
//! The KEK used is the same vault master key already in memory — no separate
//! password required.
//!
//! If the vault is locked the file is inaccessible, which is the correct
//! security behaviour: infrastructure details should only be visible to an
//! authenticated session.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rand::RngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};

use crate::config::default_config_home;
use crate::vault::VaultError;

// ─── PrivateProjectMemory ────────────────────────────────────────────────────

/// Encrypted store for sensitive infrastructure facts scoped to a project.
///
/// The on-disk format is a binary file: `[12-byte nonce][AES-256-GCM ciphertext]`.
/// The plaintext is a UTF-8 JSON object `{"key": "value", ...}`.
///
/// All mutations perform an atomic write: data is written to a sibling `.tmp`
/// file and then renamed over the target, preventing partial-write corruption.
pub struct PrivateProjectMemory {
    /// Full path to the encrypted file (`~/.anvil/private/{hash}.enc`).
    path: PathBuf,
}

impl PrivateProjectMemory {
    /// Construct a store for the given project root.
    ///
    /// The storage path is derived from the SHA-256 of the canonical absolute
    /// path so that the same project root always maps to the same file.
    #[must_use]
    pub fn for_project(project_root: &Path) -> Self {
        let canonical = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let hash = project_hash(&canonical);
        let path = default_config_home()
            .join("private")
            .join(format!("{hash}.enc"));
        Self { path }
    }

    /// Construct a store pointing at a specific path (useful for tests).
    #[must_use]
    pub fn with_path(path: PathBuf) -> Self {
        Self { path }
    }

    /// Return the on-disk path for this store (may not exist yet).
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    // ─── Core load / save ─────────────────────────────────────────────────────

    /// Decrypt and return all entries from disk.
    ///
    /// Returns an empty map if the file does not exist yet (first use).
    /// Returns `VaultError::Locked` (manifested as a `Crypto` error) if
    /// decryption fails, which callers should surface as a "vault locked" hint.
    pub fn load(&self, kek: &[u8; 32]) -> Result<HashMap<String, String>, VaultError> {
        if !self.path.exists() {
            return Ok(HashMap::new());
        }
        let cipherblob = std::fs::read(&self.path)?;
        let plaintext = decrypt_blob(kek, &cipherblob)?;
        let map: HashMap<String, String> = serde_json::from_slice(&plaintext)
            .map_err(|e| VaultError::Serialization(e.to_string()))?;
        Ok(map)
    }

    /// Encrypt and persist `entries` to disk.
    ///
    /// Uses an atomic write (temp file + rename) to avoid partial-write
    /// corruption.  The parent directory is created if it does not exist.
    pub fn save(
        &self,
        kek: &[u8; 32],
        entries: &HashMap<String, String>,
    ) -> Result<(), VaultError> {
        let plaintext = serde_json::to_vec(entries)
            .map_err(|e| VaultError::Serialization(e.to_string()))?;
        let cipherblob = encrypt_blob(kek, &plaintext)?;

        // Ensure parent directory exists.
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Atomic write: write to .tmp, then rename.
        let tmp_path = self.path.with_extension("enc.tmp");
        write_secret_file(&tmp_path, &cipherblob)?;
        std::fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }

    // ─── Convenience mutation helpers ─────────────────────────────────────────

    /// Insert or overwrite a single entry, then persist.
    pub fn add_entry(&self, kek: &[u8; 32], key: &str, value: &str) -> Result<(), VaultError> {
        let mut entries = self.load(kek)?;
        entries.insert(key.to_string(), value.to_string());
        self.save(kek, &entries)
    }

    /// Retrieve a single entry by key.  Returns `Ok(None)` if the file does
    /// not exist or the key is absent.
    pub fn get_entry(&self, kek: &[u8; 32], key: &str) -> Result<Option<String>, VaultError> {
        let entries = self.load(kek)?;
        Ok(entries.get(key).cloned())
    }

    /// Remove a single entry by key, then persist.  No-op if the key is absent.
    pub fn remove_entry(&self, kek: &[u8; 32], key: &str) -> Result<(), VaultError> {
        let mut entries = self.load(kek)?;
        entries.remove(key);
        self.save(kek, &entries)
    }

    // ─── AI context formatting ────────────────────────────────────────────────

    /// Format all entries as a structured block suitable for injection into the
    /// AI system prompt.
    ///
    /// Returns an empty string when the store is empty or the file does not
    /// exist.  On decryption failure (locked vault), returns an empty string —
    /// the caller should check vault state before calling this.
    #[must_use]
    pub fn format_for_context(&self, kek: &[u8; 32]) -> String {
        let entries = match self.load(kek) {
            Ok(e) => e,
            Err(_) => return String::new(),
        };
        if entries.is_empty() {
            return String::new();
        }

        let mut lines = vec!["# Private project context (encrypted)".to_string(), String::new()];

        // Sort keys for stable, readable output.
        let mut keys: Vec<&String> = entries.keys().collect();
        keys.sort();
        for key in keys {
            if let Some(value) = entries.get(key) {
                lines.push(format!("- **{key}**: {value}"));
            }
        }

        lines.push(String::new());
        lines.push(
            "> This context is decrypted from encrypted private project memory. \
             Do not echo these values to plaintext files."
                .to_string(),
        );

        lines.join("\n")
    }
}

// ─── Cryptographic helpers ────────────────────────────────────────────────────
//
// On-disk format: [12-byte random nonce][AES-256-GCM ciphertext + 16-byte tag]
// The KEK is used directly as the AES key — no DEK layer is needed because this
// file is per-project and the KEK itself is already the session secret.

fn encrypt_blob(kek: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>, VaultError> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(kek));
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| VaultError::Crypto(e.to_string()))?;

    // Output: nonce || ciphertext (ciphertext already includes GCM auth tag).
    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

fn decrypt_blob(kek: &[u8; 32], cipherblob: &[u8]) -> Result<Vec<u8>, VaultError> {
    if cipherblob.len() < 12 {
        return Err(VaultError::Crypto(
            "private memory blob too short to contain nonce".into(),
        ));
    }
    let (nonce_bytes, ciphertext) = cipherblob.split_at(12);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(kek));
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| VaultError::InvalidMasterPassword)
}

/// Write `data` to `path` with mode 0o600 (owner read/write only).
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
        std::fs::write(path, data)?;
    }
    Ok(())
}

// ─── Project hash ─────────────────────────────────────────────────────────────

/// Compute the full SHA-256 hex digest of the canonical project path.
///
/// Unlike `project_path_hash` in `memory.rs` which truncates to 16 hex chars
/// for display purposes, this function returns the full 64-character hex string
/// to ensure collision resistance across potentially thousands of projects.
#[must_use]
pub fn private_memory_project_hash(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    let result = hasher.finalize();
    result.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

// Internal alias for readability within this module.
fn project_hash(path: &Path) -> String {
    private_memory_project_hash(path)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::TempDir;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Create a temp directory and a `PrivateProjectMemory` whose encrypted
    /// file lives inside it.  Returns both so the temp dir is not dropped early.
    fn temp_store() -> (PrivateProjectMemory, TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let file_path = dir.path().join(format!("private-{nanos}-{seq}.enc"));
        let store = PrivateProjectMemory::with_path(file_path);
        (store, dir)
    }

    const TEST_KEK: [u8; 32] = [0u8; 32];

    // ─── Project hash ─────────────────────────────────────────────────────────

    #[test]
    fn project_hash_is_64_hex_chars() {
        let h = private_memory_project_hash(Path::new("/opt/projects/EMS-Main"));
        assert_eq!(h.len(), 64, "hash should be 64 hex chars: {h}");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()), "hash is not hex: {h}");
    }

    #[test]
    fn project_hash_is_deterministic() {
        let h1 = private_memory_project_hash(Path::new("/home/user/myproject"));
        let h2 = private_memory_project_hash(Path::new("/home/user/myproject"));
        assert_eq!(h1, h2, "same path must produce the same hash");
    }

    #[test]
    fn project_hash_differs_for_different_paths() {
        let h1 = private_memory_project_hash(Path::new("/opt/projects/EMS-Main"));
        let h2 = private_memory_project_hash(Path::new("/opt/projects/soc-integration"));
        assert_ne!(h1, h2, "different project roots must produce different hashes");
    }

    #[test]
    fn for_project_produces_different_paths_for_different_roots() {
        // Use paths that do not exist on disk (canonicalize falls back to as-is).
        let s1 = PrivateProjectMemory::for_project(Path::new("/nonexistent/project-alpha"));
        let s2 = PrivateProjectMemory::for_project(Path::new("/nonexistent/project-beta"));
        assert_ne!(
            s1.path(),
            s2.path(),
            "different project roots must map to different store files"
        );
    }

    // ─── Round-trip: save / load ───────────────────────────────────────────────

    #[test]
    fn empty_store_loads_as_empty_map() {
        let (store, _dir) = temp_store();
        let entries = store.load(&TEST_KEK).expect("load empty");
        assert!(entries.is_empty(), "fresh store should return empty map");
    }

    #[test]
    fn save_and_load_roundtrip() {
        let (store, _dir) = temp_store();
        let mut data = HashMap::new();
        data.insert("bastion_host".to_string(), "guard.armored.ninja".to_string());
        data.insert("bastion_port".to_string(), "30022".to_string());
        data.insert("deploy_path".to_string(), "/opt/projects/EMS-Main".to_string());

        store.save(&TEST_KEK, &data).expect("save");
        let loaded = store.load(&TEST_KEK).expect("load");

        assert_eq!(loaded.get("bastion_host").map(String::as_str), Some("guard.armored.ninja"));
        assert_eq!(loaded.get("bastion_port").map(String::as_str), Some("30022"));
        assert_eq!(loaded.get("deploy_path").map(String::as_str), Some("/opt/projects/EMS-Main"));
        assert_eq!(loaded.len(), 3);
    }

    #[test]
    fn wrong_key_fails_to_decrypt() {
        let (store, _dir) = temp_store();
        let mut data = HashMap::new();
        data.insert("ip".to_string(), "10.0.70.80".to_string());
        store.save(&TEST_KEK, &data).expect("save");

        let wrong_kek = [0xFFu8; 32];
        assert!(
            store.load(&wrong_kek).is_err(),
            "wrong KEK must produce a decryption error"
        );
    }

    #[test]
    fn encrypted_file_does_not_contain_plaintext() {
        let (store, _dir) = temp_store();
        let mut data = HashMap::new();
        data.insert("internal_ip".to_string(), "192.168.100.206".to_string());
        data.insert("hostname".to_string(), "dev0001.armored.ninja".to_string());
        store.save(&TEST_KEK, &data).expect("save");

        let raw = std::fs::read(store.path()).expect("read encrypted file");
        let raw_str = String::from_utf8_lossy(&raw);

        // The plaintext values must NOT appear in the binary blob.
        assert!(
            !raw_str.contains("192.168.100.206"),
            "IP address must not appear in ciphertext"
        );
        assert!(
            !raw_str.contains("dev0001.armored.ninja"),
            "hostname must not appear in ciphertext"
        );
    }

    // ─── Convenience mutation helpers ─────────────────────────────────────────

    #[test]
    fn add_and_get_entry() {
        let (store, _dir) = temp_store();
        store.add_entry(&TEST_KEK, "db_host", "10.0.70.80").expect("add");
        store.add_entry(&TEST_KEK, "db_port", "5432").expect("add");

        let val = store.get_entry(&TEST_KEK, "db_host").expect("get");
        assert_eq!(val.as_deref(), Some("10.0.70.80"));
    }

    #[test]
    fn get_entry_returns_none_for_missing_key() {
        let (store, _dir) = temp_store();
        let val = store.get_entry(&TEST_KEK, "nonexistent").expect("get");
        assert!(val.is_none(), "missing key should return None");
    }

    #[test]
    fn add_entry_overwrites_existing_key() {
        let (store, _dir) = temp_store();
        store.add_entry(&TEST_KEK, "host", "10.0.70.1").expect("add initial");
        store.add_entry(&TEST_KEK, "host", "10.0.70.2").expect("overwrite");

        let val = store.get_entry(&TEST_KEK, "host").expect("get");
        assert_eq!(val.as_deref(), Some("10.0.70.2"), "overwrite should replace old value");
    }

    #[test]
    fn remove_entry() {
        let (store, _dir) = temp_store();
        store.add_entry(&TEST_KEK, "api_url", "https://api.culpur.net").expect("add");
        store.add_entry(&TEST_KEK, "web_url", "https://bema.culpur.net").expect("add");
        store.remove_entry(&TEST_KEK, "api_url").expect("remove");

        let entries = store.load(&TEST_KEK).expect("load");
        assert!(!entries.contains_key("api_url"), "removed key must be absent");
        assert!(entries.contains_key("web_url"), "other key must remain");
    }

    #[test]
    fn remove_entry_is_noop_for_missing_key() {
        let (store, _dir) = temp_store();
        // Should not error even if key does not exist.
        store.remove_entry(&TEST_KEK, "phantom_key").expect("noop remove should not error");
    }

    // ─── Atomic write semantics ────────────────────────────────────────────────

    #[test]
    fn no_tmp_file_left_after_save() {
        let (store, _dir) = temp_store();
        let mut data = HashMap::new();
        data.insert("key".to_string(), "value".to_string());
        store.save(&TEST_KEK, &data).expect("save");

        let tmp = store.path().with_extension("enc.tmp");
        assert!(!tmp.exists(), ".tmp file must not be left on disk after successful save");
    }

    // ─── format_for_context ───────────────────────────────────────────────────

    #[test]
    fn format_for_context_empty_when_no_entries() {
        let (store, _dir) = temp_store();
        let output = store.format_for_context(&TEST_KEK);
        assert!(output.is_empty(), "empty store should produce empty context string");
    }

    #[test]
    fn format_for_context_includes_all_entries() {
        let (store, _dir) = temp_store();
        store.add_entry(&TEST_KEK, "bastion", "guard.armored.ninja").expect("add");
        store.add_entry(&TEST_KEK, "deploy_path", "/opt/projects/EMS-Main").expect("add");

        let output = store.format_for_context(&TEST_KEK);
        assert!(output.contains("# Private project context"), "should have header");
        assert!(output.contains("bastion"), "should contain key name");
        assert!(output.contains("guard.armored.ninja"), "should contain value");
        assert!(output.contains("deploy_path"), "should contain key name");
        assert!(output.contains("/opt/projects/EMS-Main"), "should contain value");
        assert!(output.contains("Do not echo"), "should contain redaction reminder");
    }

    #[test]
    fn format_for_context_keys_are_sorted() {
        let (store, _dir) = temp_store();
        store.add_entry(&TEST_KEK, "zebra", "z-val").expect("add");
        store.add_entry(&TEST_KEK, "alpha", "a-val").expect("add");
        store.add_entry(&TEST_KEK, "mango", "m-val").expect("add");

        let output = store.format_for_context(&TEST_KEK);
        let alpha_pos = output.find("alpha").expect("alpha in output");
        let mango_pos = output.find("mango").expect("mango in output");
        let zebra_pos = output.find("zebra").expect("zebra in output");
        assert!(alpha_pos < mango_pos, "alpha should appear before mango");
        assert!(mango_pos < zebra_pos, "mango should appear before zebra");
    }

    #[test]
    fn format_for_context_returns_empty_on_wrong_key() {
        let (store, _dir) = temp_store();
        store.add_entry(&TEST_KEK, "host", "10.0.0.1").expect("add");

        let wrong_kek = [0xAAu8; 32];
        let output = store.format_for_context(&wrong_kek);
        assert!(
            output.is_empty(),
            "wrong KEK should silently return empty string (vault locked semantics)"
        );
    }

    // ─── Different project roots → different files ────────────────────────────

    #[test]
    fn different_project_roots_produce_different_storage_files() {
        let dir = tempfile::tempdir().expect("tempdir");

        let s1 = PrivateProjectMemory::with_path(
            dir.path().join(format!("{}.enc", private_memory_project_hash(Path::new("/proj/a")))),
        );
        let s2 = PrivateProjectMemory::with_path(
            dir.path().join(format!("{}.enc", private_memory_project_hash(Path::new("/proj/b")))),
        );

        s1.add_entry(&TEST_KEK, "host", "10.0.0.1").expect("add s1");
        s2.add_entry(&TEST_KEK, "host", "10.0.0.2").expect("add s2");

        let v1 = s1.get_entry(&TEST_KEK, "host").expect("get s1");
        let v2 = s2.get_entry(&TEST_KEK, "host").expect("get s2");

        assert_eq!(v1.as_deref(), Some("10.0.0.1"));
        assert_eq!(v2.as_deref(), Some("10.0.0.2"));
        assert_ne!(s1.path(), s2.path(), "each project must have its own storage file");
    }

    // ─── Infrastructure entry set: realistic usage ────────────────────────────

    #[test]
    fn realistic_infrastructure_entries_roundtrip() {
        let (store, _dir) = temp_store();

        let infra = [
            ("bastion_host", "guard.armored.ninja"),
            ("bastion_port", "30022"),
            ("bastion_user", "soulofall"),
            ("dev_server_ip", "10.0.70.80"),
            ("deploy_path", "/opt/projects/EMS-Main"),
            ("api_port", "4501"),
            ("db_name", "ems_api"),
            ("proxmox_api", "https://node0001.culpur.net:8006"),
        ];

        for (k, v) in &infra {
            store.add_entry(&TEST_KEK, k, v).expect("add entry");
        }

        let entries = store.load(&TEST_KEK).expect("load");
        assert_eq!(entries.len(), infra.len());

        for (k, v) in &infra {
            assert_eq!(
                entries.get(*k).map(String::as_str),
                Some(*v),
                "entry {k} mismatch"
            );
        }
    }
}
