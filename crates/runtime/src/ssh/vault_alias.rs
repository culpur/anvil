//! Vault adapter for SSH aliases (T5-Ssh-C).
//!
//! Reuses the existing `CredentialType::HostCredential` slot in the vault.
//! An SSH alias maps onto `Credential` like this:
//!
//! | Credential field | SSH meaning                                          |
//! |------------------|------------------------------------------------------|
//! | `label`          | the alias the user picks (e.g. `"guard"`)            |
//! | `credential_type`| `HostCredential`                                     |
//! | `username`       | ssh user                                             |
//! | `secret`         | password OR key passphrase OR `""` for agent-only    |
//! | `url`            | `host:port`                                          |
//! | `metadata`       | JSON object (see `SshMetadata` below)                |
//!
//! `metadata` carries the auth-method discriminator and (for key auth) the
//! path to the private key file. We keep the file path in metadata rather
//! than reading the key into `secret` because keys can be huge and the
//! user usually wants ssh-agent or `~/.ssh/id_*` to do the actual signing.
//!
//! Storing a password literally in `secret` is fine — that's exactly what
//! the AES-256-GCM envelope is for.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::vault::{Credential, CredentialType, VaultError, VaultManager};

use super::config::{SshAuthMethod, SshConfig};

/// Auth-method discriminator stored in `Credential.metadata.ssh_auth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshAuthKind {
    Agent,
    Key,
    Password,
    Interactive,
}

/// Shape of `Credential.metadata` for an SSH alias. Stored as a JSON object;
/// `serde_json::Value::Object` round-trips cleanly through the existing
/// vault schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshMetadata {
    /// Always `"ssh"` so future readers can identify host-credential entries
    /// that came from `/ssh save` versus older host entries.
    pub kind: String,
    /// Which auth method to drive on connect.
    pub ssh_auth: SshAuthKind,
    /// Filesystem path to the private key — only meaningful when
    /// `ssh_auth == Key`. Stored as a string so it round-trips through JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
}

impl SshMetadata {
    fn for_method(auth: &SshAuthMethod) -> Self {
        match auth {
            SshAuthMethod::Agent => Self {
                kind: "ssh".to_string(),
                ssh_auth: SshAuthKind::Agent,
                key_path: None,
            },
            SshAuthMethod::KeyFile { path, .. } => Self {
                kind: "ssh".to_string(),
                ssh_auth: SshAuthKind::Key,
                key_path: Some(path.display().to_string()),
            },
            SshAuthMethod::Password(_) => Self {
                kind: "ssh".to_string(),
                ssh_auth: SshAuthKind::Password,
                key_path: None,
            },
            SshAuthMethod::KeyboardInteractive => Self {
                kind: "ssh".to_string(),
                ssh_auth: SshAuthKind::Interactive,
                key_path: None,
            },
        }
    }
}

/// Errors specific to SSH-alias vault operations. Wraps `VaultError` for
/// pass-through and adds variants for SSH-shape problems.
#[derive(Debug)]
pub enum SshAliasError {
    Vault(VaultError),
    /// Existing credential is in the vault but not shaped like an SSH alias
    /// (missing/invalid metadata, wrong type, malformed url).
    NotAnSshAlias(String),
    /// `host:port` field couldn't be parsed.
    BadUrl(String),
    /// Vault stored a key path but the file is gone.
    KeyFileMissing(String),
}

impl std::fmt::Display for SshAliasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Vault(e) => write!(f, "vault: {e}"),
            Self::NotAnSshAlias(msg) => write!(f, "not an SSH alias: {msg}"),
            Self::BadUrl(msg) => write!(f, "bad host:port in alias: {msg}"),
            Self::KeyFileMissing(p) => write!(f, "key file missing: {p}"),
        }
    }
}

impl std::error::Error for SshAliasError {}

impl From<VaultError> for SshAliasError {
    fn from(e: VaultError) -> Self {
        Self::Vault(e)
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Save an SSH alias to the vault. Overwrites any existing credential with
/// the same label (upsert) — `/ssh save <alias>` should be idempotent so
/// users can update host details without first deleting the old entry.
///
/// Vault must be unlocked.
pub fn save_ssh_alias(
    vault: &VaultManager,
    label: &str,
    config: &SshConfig,
) -> Result<(), SshAliasError> {
    let metadata = SshMetadata::for_method(&config.auth);
    let metadata_json = serde_json::to_value(&metadata)
        .map_err(|e| SshAliasError::Vault(VaultError::Serialization(e.to_string())))?;

    // Pull the secret material into the vault `secret` field. We store
    // either the password or the key passphrase verbatim; the key file
    // itself is referenced via metadata.key_path. Empty string for
    // Agent / Interactive (no secret to keep).
    let secret = match &config.auth {
        SshAuthMethod::Agent | SshAuthMethod::KeyboardInteractive => String::new(),
        SshAuthMethod::Password(p) => p.clone(),
        SshAuthMethod::KeyFile { passphrase, .. } => passphrase.clone().unwrap_or_default(),
    };

    let now = now_secs();
    let cred = Credential {
        label: label.to_string(),
        credential_type: CredentialType::HostCredential,
        username: Some(config.user.clone()),
        secret,
        url: Some(format!("{}:{}", config.host, config.port)),
        notes: None,
        tags: vec!["ssh".to_string()],
        created_at: now,
        updated_at: now,
        expires_at: None,
        last_rotated: None,
        metadata: metadata_json,
    };

    vault.upsert_credential(&cred)?;
    Ok(())
}

/// Load an SSH alias from the vault and reconstruct an [`SshConfig`] ready
/// to feed to `runtime::ssh::connect`.
///
/// Returns:
///   - `SshAliasError::NotAnSshAlias` if the credential exists but isn't
///     shaped like an SSH alias.
///   - `SshAliasError::Vault(VaultError::NotFound)` if no credential
///     exists with this label.
///
/// Vault must be unlocked.
pub fn load_ssh_alias(
    vault: &VaultManager,
    label: &str,
) -> Result<SshConfig, SshAliasError> {
    let cred = vault.get_credential(label)?;

    if cred.credential_type != CredentialType::HostCredential {
        return Err(SshAliasError::NotAnSshAlias(format!(
            "credential type is {:?}, not HostCredential",
            cred.credential_type
        )));
    }

    let metadata: SshMetadata = serde_json::from_value(cred.metadata.clone()).map_err(|_| {
        SshAliasError::NotAnSshAlias(format!(
            "metadata for {label:?} doesn't match SshMetadata schema"
        ))
    })?;

    if metadata.kind != "ssh" {
        return Err(SshAliasError::NotAnSshAlias(format!(
            "metadata.kind is {:?}, not \"ssh\"",
            metadata.kind
        )));
    }

    // Parse host:port from `url`.
    let url = cred
        .url
        .as_deref()
        .ok_or_else(|| SshAliasError::NotAnSshAlias("missing url".to_string()))?;
    let (host, port) = parse_host_port(url)?;

    let user = cred
        .username
        .clone()
        .ok_or_else(|| SshAliasError::NotAnSshAlias("missing username".to_string()))?;

    let auth = match metadata.ssh_auth {
        SshAuthKind::Agent => SshAuthMethod::Agent,
        SshAuthKind::Password => SshAuthMethod::Password(cred.secret.clone()),
        SshAuthKind::Interactive => SshAuthMethod::KeyboardInteractive,
        SshAuthKind::Key => {
            let key_path = metadata
                .key_path
                .as_deref()
                .ok_or_else(|| {
                    SshAliasError::NotAnSshAlias("key auth but no key_path".to_string())
                })?;
            let path = PathBuf::from(key_path);
            if !path.exists() {
                return Err(SshAliasError::KeyFileMissing(key_path.to_string()));
            }
            let passphrase = if cred.secret.is_empty() {
                None
            } else {
                Some(cred.secret.clone())
            };
            SshAuthMethod::KeyFile { path, passphrase }
        }
    };

    Ok(SshConfig {
        host,
        port,
        user,
        auth,
    })
}

/// List labels of all SSH aliases currently in the vault. Vault must be
/// unlocked. Used to render an alias picker if we ever surface one.
pub fn list_ssh_aliases(vault: &VaultManager) -> Result<Vec<String>, SshAliasError> {
    let mut out = Vec::new();
    for label in vault.list_credentials()? {
        if let Ok(cred) = vault.get_credential(&label)
            && cred.credential_type == CredentialType::HostCredential
            && cred.tags.iter().any(|t| t == "ssh")
        {
            out.push(label);
        }
    }
    Ok(out)
}

fn parse_host_port(url: &str) -> Result<(String, u16), SshAliasError> {
    // Accept either `host:port` or just `host` (default port 22).
    if let Some((h, p)) = url.rsplit_once(':') {
        if let Ok(port) = p.parse::<u16>() {
            return Ok((h.to_string(), port));
        }
    }
    if !url.is_empty() && !url.contains(':') {
        return Ok((url.to_string(), 22));
    }
    Err(SshAliasError::BadUrl(url.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    fn unlocked_vault(dir: &Path) -> VaultManager {
        let mut v = VaultManager::new(dir.to_path_buf());
        v.setup("test-pw").expect("setup vault");
        v.unlock("test-pw").expect("unlock vault");
        v
    }

    #[test]
    fn parse_host_port_accepts_both_shapes() {
        assert_eq!(parse_host_port("guard:30022").unwrap(), ("guard".to_string(), 30022));
        assert_eq!(parse_host_port("10.0.70.80:22").unwrap(), ("10.0.70.80".to_string(), 22));
        assert_eq!(parse_host_port("guard").unwrap(), ("guard".to_string(), 22));
    }

    #[test]
    fn parse_host_port_rejects_empty() {
        assert!(matches!(parse_host_port(""), Err(SshAliasError::BadUrl(_))));
    }

    #[test]
    fn round_trip_password_alias() {
        let tmp = TempDir::new().unwrap();
        let v = unlocked_vault(tmp.path());
        let cfg = SshConfig {
            host: "guard.example.net".into(),
            port: 30022,
            user: "soulofall".into(),
            auth: SshAuthMethod::Password("hunter2".into()),
        };
        save_ssh_alias(&v, "guard", &cfg).unwrap();
        let loaded = load_ssh_alias(&v, "guard").unwrap();
        assert_eq!(loaded.host, "guard.example.net");
        assert_eq!(loaded.port, 30022);
        assert_eq!(loaded.user, "soulofall");
        match loaded.auth {
            SshAuthMethod::Password(p) => assert_eq!(p, "hunter2"),
            other => panic!("wrong auth: {other:?}"),
        }
    }

    #[test]
    fn round_trip_agent_alias_carries_no_secret() {
        let tmp = TempDir::new().unwrap();
        let v = unlocked_vault(tmp.path());
        let cfg = SshConfig {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: SshAuthMethod::Agent,
        };
        save_ssh_alias(&v, "h", &cfg).unwrap();
        let loaded = load_ssh_alias(&v, "h").unwrap();
        assert!(matches!(loaded.auth, SshAuthMethod::Agent));
    }

    #[test]
    fn round_trip_key_alias_with_passphrase() {
        let tmp = TempDir::new().unwrap();
        let v = unlocked_vault(tmp.path());
        // Touch a fake key file so load doesn't error on missing file.
        let key_path = tmp.path().join("id_ed25519");
        std::fs::write(&key_path, "fake key bytes").unwrap();
        let cfg = SshConfig {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: SshAuthMethod::KeyFile {
                path: key_path.clone(),
                passphrase: Some("phrase".into()),
            },
        };
        save_ssh_alias(&v, "h", &cfg).unwrap();
        let loaded = load_ssh_alias(&v, "h").unwrap();
        match loaded.auth {
            SshAuthMethod::KeyFile { path, passphrase } => {
                assert_eq!(path, key_path);
                assert_eq!(passphrase.as_deref(), Some("phrase"));
            }
            other => panic!("wrong auth: {other:?}"),
        }
    }

    #[test]
    fn load_missing_key_file_errors() {
        let tmp = TempDir::new().unwrap();
        let v = unlocked_vault(tmp.path());
        let key_path = tmp.path().join("nonexistent");
        let cfg = SshConfig {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: SshAuthMethod::KeyFile {
                path: key_path,
                passphrase: None,
            },
        };
        save_ssh_alias(&v, "h", &cfg).unwrap();
        match load_ssh_alias(&v, "h") {
            Err(SshAliasError::KeyFileMissing(_)) => {}
            other => panic!("expected KeyFileMissing, got {other:?}"),
        }
    }

    #[test]
    fn save_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let v = unlocked_vault(tmp.path());
        let cfg1 = SshConfig {
            host: "h1".into(),
            port: 22,
            user: "u".into(),
            auth: SshAuthMethod::Agent,
        };
        let cfg2 = SshConfig {
            host: "h2".into(),
            port: 2222,
            user: "u2".into(),
            auth: SshAuthMethod::Password("p".into()),
        };
        save_ssh_alias(&v, "alias", &cfg1).unwrap();
        save_ssh_alias(&v, "alias", &cfg2).unwrap(); // overwrites
        let loaded = load_ssh_alias(&v, "alias").unwrap();
        assert_eq!(loaded.host, "h2");
        assert_eq!(loaded.port, 2222);
        assert_eq!(loaded.user, "u2");
    }

    #[test]
    fn list_ssh_aliases_filters_by_tag() {
        let tmp = TempDir::new().unwrap();
        let v = unlocked_vault(tmp.path());
        save_ssh_alias(
            &v,
            "ssh-one",
            &SshConfig {
                host: "h".into(),
                port: 22,
                user: "u".into(),
                auth: SshAuthMethod::Agent,
            },
        )
        .unwrap();
        save_ssh_alias(
            &v,
            "ssh-two",
            &SshConfig {
                host: "h2".into(),
                port: 22,
                user: "u".into(),
                auth: SshAuthMethod::Agent,
            },
        )
        .unwrap();
        // Add a non-SSH credential to confirm it's filtered out.
        v.upsert_credential(&Credential {
            label: "api-key".into(),
            credential_type: CredentialType::ApiKey,
            username: None,
            secret: "k".into(),
            url: None,
            notes: None,
            tags: vec![],
            created_at: 0,
            updated_at: 0,
            expires_at: None,
            last_rotated: None,
            metadata: serde_json::Value::Null,
        })
        .unwrap();

        let mut aliases = list_ssh_aliases(&v).unwrap();
        aliases.sort();
        assert_eq!(aliases, vec!["ssh-one".to_string(), "ssh-two".to_string()]);
    }

    #[test]
    fn load_rejects_non_ssh_host_credential() {
        let tmp = TempDir::new().unwrap();
        let v = unlocked_vault(tmp.path());
        // Manually create a HostCredential without the ssh metadata shape.
        v.upsert_credential(&Credential {
            label: "legacy".into(),
            credential_type: CredentialType::HostCredential,
            username: Some("u".into()),
            secret: "p".into(),
            url: Some("h:22".into()),
            notes: None,
            tags: vec![],
            created_at: 0,
            updated_at: 0,
            expires_at: None,
            last_rotated: None,
            metadata: serde_json::json!({"unrelated": "data"}),
        })
        .unwrap();

        match load_ssh_alias(&v, "legacy") {
            Err(SshAliasError::NotAnSshAlias(_)) => {}
            other => panic!("expected NotAnSshAlias, got {other:?}"),
        }
    }
}
