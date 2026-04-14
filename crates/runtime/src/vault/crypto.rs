//! Cryptographic primitives: AES-256-GCM encryption/decryption, Argon2id key
//! derivation, HOTP/TOTP generation, and TOTP URI parsing.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::{Argon2, Params};
use base32::Alphabet;
use hmac::Hmac;
use rand::RngCore;
use rand::rngs::OsRng;
use sha1::Sha1;

use super::{TotpEntry, VaultError};
use super::storage::EncryptedEnvelope;

// ─── Key derivation ───────────────────────────────────────────────────────────

/// Derive a 32-byte KEK from a password + salt using Argon2id.
pub(super) fn derive_key(
    password: &str,
    salt_str: &str,
    params: &Params,
) -> Result<[u8; 32], VaultError> {
    use argon2::Algorithm;
    use argon2::Version;

    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params.clone());
    let mut key = [0u8; 32];
    argon2
        .hash_password_into(password.as_bytes(), salt_str.as_bytes(), &mut key)
        .map_err(|e| VaultError::Crypto(e.to_string()))?;
    Ok(key)
}

// ─── AES-256-GCM ──────────────────────────────────────────────────────────────

/// Encrypt `plaintext` with AES-256-GCM using the given key.
/// Returns `(nonce_bytes, ciphertext_bytes)`.
pub(super) fn aes_encrypt(
    key: &[u8; 32],
    plaintext: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), VaultError> {
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
pub(super) fn aes_decrypt(
    key: &[u8; 32],
    nonce_bytes: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, VaultError> {
    let cipher_key = Key::<Aes256Gcm>::from_slice(key);
    let cipher = Aes256Gcm::new(cipher_key);
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| VaultError::Crypto(e.to_string()))
}

// ─── Envelope encryption ──────────────────────────────────────────────────────

/// Build an `EncryptedEnvelope` using envelope encryption:
/// 1. Generate a random 32-byte DEK.
/// 2. Encrypt `plaintext` with the DEK.
/// 3. Encrypt the DEK with the KEK.
pub(super) fn build_envelope(
    kek: &[u8; 32],
    label: &str,
    plaintext: &[u8],
) -> Result<EncryptedEnvelope, VaultError> {
    // Generate DEK.
    let mut dek = [0u8; 32];
    OsRng.fill_bytes(&mut dek);

    // Encrypt data with DEK.
    let (data_nonce, data_ct) = aes_encrypt(&dek, plaintext)?;

    // Encrypt DEK with KEK.
    let (dek_nonce, dek_ct) = aes_encrypt(kek, &dek)?;

    // Zero DEK from stack.
    dek.fill(0);

    Ok(EncryptedEnvelope {
        label: label.to_string(),
        dek_nonce: base64_encode(&dek_nonce),
        dek_ciphertext: base64_encode(&dek_ct),
        data_nonce: base64_encode(&data_nonce),
        data_ciphertext: base64_encode(&data_ct),
    })
}

/// Decrypt an `EncryptedEnvelope` in memory (takes already-loaded envelope).
pub(super) fn open_envelope_data(
    kek: &[u8; 32],
    envelope: &EncryptedEnvelope,
) -> Result<Vec<u8>, VaultError> {
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

    dek.fill(0);
    Ok(plaintext)
}

// ─── Base64 helpers ───────────────────────────────────────────────────────────

pub(super) fn base64_encode(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    STANDARD.encode(bytes)
}

pub(super) fn base64_decode(s: &str) -> Result<Vec<u8>, VaultError> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    STANDARD
        .decode(s.trim())
        .map_err(|e| VaultError::Crypto(format!("base64 decode: {e}")))
}

// ─── HOTP / TOTP ─────────────────────────────────────────────────────────────

/// HOTP (RFC 4226) — compute a 6-digit code for a given counter.
pub(super) fn hotp(secret: &[u8], counter: u64) -> Result<u32, VaultError> {
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

/// Generate the current TOTP code for a given `TotpEntry`.
/// Returns `(six_digit_code_string, remaining_secs)`.
pub(super) fn generate_totp_code(entry: &TotpEntry) -> Result<(String, u64), VaultError> {
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
    Ok((format!("{code:06}"), remaining))
}

// ─── TOTP URI parsing ─────────────────────────────────────────────────────────

/// Parse an `otpauth://totp/...` URI into a `TotpEntry`.
pub(super) fn parse_otpauth_uri(label: &str, uri: &str) -> Result<TotpEntry, VaultError> {
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

// ─── SSH Key Parsing ────────────────────────────────────────────────────────

/// Parse an SSH private key and extract metadata.
pub fn parse_ssh_key(content: &str) -> serde_json::Value {
    let mut info = serde_json::Map::new();

    // Detect key type from PEM header
    let key_type = if content.contains("BEGIN OPENSSH PRIVATE KEY") {
        // Could be ed25519, RSA, ECDSA — need to check further
        if content.len() < 500 { "ed25519" } else if content.len() < 2000 { "ecdsa" } else { "rsa" }
    } else if content.contains("BEGIN RSA PRIVATE KEY") {
        "rsa"
    } else if content.contains("BEGIN EC PRIVATE KEY") {
        "ecdsa"
    } else if content.contains("BEGIN DSA PRIVATE KEY") {
        "dsa"
    } else {
        "unknown"
    };
    info.insert("key_type".into(), serde_json::Value::String(key_type.into()));

    // Try to extract fingerprint using ssh-keygen
    if let Ok(fingerprint) = ssh_key_fingerprint(content) {
        info.insert("fingerprint".into(), serde_json::Value::String(fingerprint));
    }

    // Extract comment if present (last line before END marker sometimes has it)
    let lines: Vec<&str> = content.lines().collect();
    if let Some(comment_line) = lines.iter().find(|l| l.starts_with("Comment:")) {
        info.insert("comment".into(), serde_json::Value::String(
            comment_line.trim_start_matches("Comment:").trim().trim_matches('"').to_string()
        ));
    }

    serde_json::Value::Object(info)
}

/// Compute SSH key fingerprint using ssh-keygen subprocess.
fn ssh_key_fingerprint(key_content: &str) -> Result<String, String> {
    use std::io::Write;
    let mut child = std::process::Command::new("ssh-keygen")
        .args(["-lf", "/dev/stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(key_content.as_bytes());
    }

    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if output.status.success() {
        let line = String::from_utf8_lossy(&output.stdout);
        // Format: "256 SHA256:abc123... comment (ED25519)"
        Ok(line.trim().to_string())
    } else {
        Err("ssh-keygen failed".into())
    }
}

// ─── X.509 Certificate Parsing ──────────────────────────────────────────────

/// Parse a PEM-encoded X.509 certificate and extract metadata.
pub fn parse_x509_cert(pem_content: &str) -> serde_json::Value {
    let mut info = serde_json::Map::new();

    // Use openssl subprocess to extract metadata
    if let Ok(subject) = openssl_x509_field(pem_content, "-subject") {
        // Parse CN from subject line: "subject= /CN=*.culpur.net"
        if let Some(cn) = subject.split("CN=").nth(1) {
            let cn = cn.split('/').next().unwrap_or(cn).trim();
            info.insert("cn".into(), serde_json::Value::String(cn.to_string()));
        }
        info.insert("subject".into(), serde_json::Value::String(subject));
    }

    if let Ok(issuer) = openssl_x509_field(pem_content, "-issuer") {
        if let Some(issuer_cn) = issuer.split("CN=").nth(1) {
            let issuer_cn = issuer_cn.split('/').next().unwrap_or(issuer_cn).trim();
            info.insert("issuer".into(), serde_json::Value::String(issuer_cn.to_string()));
        }
    }

    if let Ok(dates) = openssl_x509_field(pem_content, "-dates") {
        for line in dates.lines() {
            if line.starts_with("notAfter=") {
                info.insert("not_after".into(), serde_json::Value::String(
                    line.trim_start_matches("notAfter=").trim().to_string()
                ));
            }
            if line.starts_with("notBefore=") {
                info.insert("not_before".into(), serde_json::Value::String(
                    line.trim_start_matches("notBefore=").trim().to_string()
                ));
            }
        }
    }

    if let Ok(sans) = openssl_x509_field(pem_content, "-ext subjectAltName") {
        let san_list: Vec<String> = sans.lines()
            .filter(|l| l.contains("DNS:"))
            .flat_map(|l| l.split(','))
            .map(|s| s.trim().trim_start_matches("DNS:").to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !san_list.is_empty() {
            info.insert("sans".into(), serde_json::json!(san_list));
        }
    }

    // Check if it's a private key too
    info.insert("has_private_key".into(), serde_json::Value::Bool(
        pem_content.contains("PRIVATE KEY")
    ));

    serde_json::Value::Object(info)
}

/// Run openssl x509 with a specific flag and return stdout.
fn openssl_x509_field(pem: &str, flag: &str) -> Result<String, String> {
    use std::io::Write;
    let args: Vec<&str> = std::iter::once("x509")
        .chain(std::iter::once("-noout"))
        .chain(flag.split_whitespace())
        .collect();
    let mut child = std::process::Command::new("openssl")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(pem.as_bytes());
    }

    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err("openssl failed".into())
    }
}

// ─── Database URL Parsing ───────────────────────────────────────────────────

/// Parse a database connection URL and extract metadata.
pub fn parse_database_url(url: &str) -> serde_json::Value {
    let mut info = serde_json::Map::new();

    // Format: engine://user:pass@host:port/database?params
    if let Some(engine_end) = url.find("://") {
        let engine = &url[..engine_end];
        info.insert("engine".into(), serde_json::Value::String(engine.to_string()));

        let rest = &url[engine_end + 3..];
        // Split at @ for auth vs host
        if let Some(at_pos) = rest.find('@') {
            let host_part = &rest[at_pos + 1..];
            // Split host:port/database
            let (host_port, database) = if let Some(slash) = host_part.find('/') {
                (&host_part[..slash], host_part[slash + 1..].split('?').next().unwrap_or(""))
            } else {
                (host_part, "")
            };
            let (host, port) = if let Some(colon) = host_port.rfind(':') {
                (&host_port[..colon], host_port[colon + 1..].parse::<u16>().ok())
            } else {
                (host_port, None)
            };
            info.insert("host".into(), serde_json::Value::String(host.to_string()));
            if let Some(p) = port {
                info.insert("port".into(), serde_json::json!(p));
            }
            if !database.is_empty() {
                info.insert("database".into(), serde_json::Value::String(database.to_string()));
            }
        }

        info.insert("ssl".into(), serde_json::Value::Bool(url.contains("ssl=true") || url.contains("sslmode=")));
    }

    serde_json::Value::Object(info)
}
