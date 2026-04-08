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
    dek.iter_mut().for_each(|b| *b = 0);

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

    dek.iter_mut().for_each(|b| *b = 0);
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
