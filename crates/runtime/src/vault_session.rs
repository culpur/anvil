//! Session-scoped vault cache.
//!
//! Stores the KEK bytes in a process-global `OnceLock` so the master password
//! is prompted exactly once per Anvil session.  The cache is populated by
//! `init_session_vault` (called at startup) and read by
//! `with_session_vault` wherever vault credentials are needed.
//!
//! Security properties:
//! - The password itself is never retained — only the 32-byte derived KEK.
//! - The KEK lives solely in heap memory and is zeroed on Drop via
//!   `VaultManager::lock()`.
//! - The `OnceLock` is never exposed outside this module; callers only receive
//!   a reference through the `with_session_vault` closure.

use std::sync::{Mutex, OnceLock};

use crate::vault::{Credential, VaultError, VaultManager};

/// Global process-level vault session, lazily initialised.
static SESSION_VAULT: OnceLock<Mutex<VaultManager>> = OnceLock::new();

/// Initialise (or re-use) the session vault.
///
/// If the vault is not yet initialized this function returns `Ok(false)` and
/// does nothing — the caller should run the wizard to create the vault first.
///
/// If the vault is already initialized but still locked, this function unlocks
/// it with `password` and stores the unlocked manager in the global slot.
///
/// If the global slot is already populated (vault already unlocked for this
/// session), the password argument is ignored and the function returns
/// `Ok(true)`.
pub fn init_session_vault(password: &str) -> Result<bool, VaultError> {
    // Fast path: already unlocked this session.
    if let Some(mutex) = SESSION_VAULT.get() {
        let guard = mutex.lock().map_err(|_| VaultError::Crypto("lock poisoned".into()))?;
        if guard.is_unlocked() {
            return Ok(true);
        }
        drop(guard);
    }

    let mut vm = VaultManager::with_default_dir();
    if !vm.is_initialized() {
        return Ok(false);
    }
    vm.unlock(password)?;

    // Install into global slot (best-effort — another thread may have beaten
    // us to it, which is fine since both unlocked the same vault).
    let _ = SESSION_VAULT.set(Mutex::new(vm));

    // If set() failed the slot already holds an unlocked manager; that is
    // equally good — the vault is unlocked for this session.
    Ok(true)
}

/// Returns `true` if the vault is initialized on disk.
pub fn vault_is_initialized() -> bool {
    VaultManager::with_default_dir().is_initialized()
}

/// Returns `true` if the session vault has been unlocked.
pub fn vault_is_session_unlocked() -> bool {
    SESSION_VAULT
        .get()
        .and_then(|m| m.lock().ok())
        .map_or(false, |g| g.is_unlocked())
}

/// Execute `f` with a reference to the unlocked `VaultManager`.
///
/// Returns `Err(VaultError::Locked)` if the vault has not been unlocked for
/// this session yet.
pub fn with_session_vault<F, T>(f: F) -> Result<T, VaultError>
where
    F: FnOnce(&VaultManager) -> Result<T, VaultError>,
{
    let mutex = SESSION_VAULT.get().ok_or(VaultError::Locked)?;
    let guard = mutex.lock().map_err(|_| VaultError::Crypto("lock poisoned".into()))?;
    if !guard.is_unlocked() {
        return Err(VaultError::Locked);
    }
    f(&guard)
}

/// Execute `f` with a mutable reference to the unlocked `VaultManager`.
pub fn with_session_vault_mut<F, T>(f: F) -> Result<T, VaultError>
where
    F: FnOnce(&mut VaultManager) -> Result<T, VaultError>,
{
    let mutex = SESSION_VAULT.get().ok_or(VaultError::Locked)?;
    let mut guard = mutex.lock().map_err(|_| VaultError::Crypto("lock poisoned".into()))?;
    if !guard.is_unlocked() {
        return Err(VaultError::Locked);
    }
    f(&mut guard)
}

/// Convenience: store or overwrite a simple key/value credential in the
/// session vault.  Does nothing if the vault is not unlocked this session.
pub fn vault_session_upsert(label: &str, secret: &str) -> Result<(), VaultError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cred = Credential {
        label: label.to_string(),
        username: None,
        secret: secret.to_string(),
        notes: Some("Set during Anvil session".to_string()),
        created_at: now,
    };
    with_session_vault(|vm| vm.upsert_credential(&cred))
}

/// Convenience: retrieve a credential secret from the session vault.
/// Returns `None` if vault is locked or the label does not exist.
pub fn vault_session_get(label: &str) -> Option<String> {
    with_session_vault(|vm| vm.get_credential(label))
        .ok()
        .map(|c| c.secret)
}
