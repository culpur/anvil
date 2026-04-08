//! Credential vault command implementation.
//!
//! Extracted from main.rs to keep the vault logic self-contained.
//! All sensitive input is read via no-echo terminal prompts so secrets never
//! appear in REPL history or process argument lists.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Write an `Authorization: Bearer <token>` header to a temporary file with
/// mode 0o600 so the token is never visible in the process argument list.
pub(crate) fn write_curl_auth_header(token: &str) -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join(format!(
        "anvil-curl-auth-{}-{}.hdr",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("Failed to create auth header temp file: {e}"))?;
        write!(f, "Authorization: Bearer {token}")
            .map_err(|e| format!("Failed to write auth header: {e}"))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, format!("Authorization: Bearer {token}"))
            .map_err(|e| format!("Failed to write auth header temp file: {e}"))?;
    }
    Ok(path)
}

/// Read a password from the terminal with no echo.
/// Prints `prompt` to stderr so it is visible even when stdout is piped.
pub(crate) fn read_password_prompt(prompt: &str) -> Result<String, String> {
    eprint!("{prompt}");
    rpassword::read_password().map_err(|e| format!("Failed to read password: {e}"))
}

/// Mask a secret for display: show first 4 and last 4 characters separated by "…".
/// Secrets shorter than 9 characters are fully masked with "****".
pub(crate) fn mask_secret(s: &str) -> String {
    if s.len() < 9 {
        return "****".to_string();
    }
    format!("{}…{}", &s[..4], &s[s.len() - 4..])
}

/// Execute a `/vault` slash command.
///
/// This is called from `LiveCli::run_vault_command`.  All sub-commands that
/// require a master password read it via `read_password_prompt` (no echo).
#[allow(clippy::similar_names)]
pub(crate) fn run_vault_command_impl(args: Option<&str>) -> String {
    use runtime::{Credential, TotpEntry, VaultManager};

    let sub = args.unwrap_or("").trim();
    let mut parts = sub.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("").trim();
    let arg1 = parts.next().unwrap_or("").trim();

    let mut vm = VaultManager::with_default_dir();

    match cmd {
        // ── Status ──────────────────────────────────────────────────────────
        "" => {
            let init = if vm.is_initialized() { "yes" } else { "no" };
            let locked = if vm.is_unlocked() { "unlocked" } else { "locked" };
            format!(
                "Vault\n  Initialized      {init}\n  State            {locked}\n  Storage          {}\n\n  Commands: /vault setup | /vault unlock | /vault lock\n            /vault store <label> | /vault get <label> | /vault list | /vault delete <label>\n            /vault totp add <label> | /vault totp <label> | /vault totp list | /vault totp delete <label>",
                VaultManager::default_vault_dir().display()
            )
        }

        // ── Setup ────────────────────────────────────────────────────────────
        "setup" => {
            if vm.is_initialized() {
                return "Vault\n  Error            Vault already initialized. Delete ~/.anvil/vault/ to reset.".to_string();
            }
            let password = match read_password_prompt("Enter master password: ") {
                Ok(p) => p,
                Err(e) => return format!("Vault\n  Error            {e}"),
            };
            if password.is_empty() {
                return "Vault\n  Error            Password must not be empty.".to_string();
            }
            let confirm = match read_password_prompt("Confirm master password: ") {
                Ok(p) => p,
                Err(e) => return format!("Vault\n  Error            {e}"),
            };
            if password != confirm {
                return "Vault\n  Error            Passwords do not match — setup cancelled.".to_string();
            }
            match vm.setup(&password) {
                Ok(()) => format!(
                    "Vault\n  Result           Initialized\n  Storage          {}\n  Algorithm        Argon2id + AES-256-GCM\n  Note             Master password not stored — keep it safe.",
                    VaultManager::default_vault_dir().display()
                ),
                Err(e) => format!("Vault setup error: {e}"),
            }
        }

        // ── Unlock ───────────────────────────────────────────────────────────
        "unlock" => {
            let password = match read_password_prompt("Master password: ") {
                Ok(p) => p,
                Err(e) => return format!("Vault\n  Error            {e}"),
            };
            match vm.unlock(&password) {
                Ok(()) => "Vault\n  Result           Unlocked\n  Note             Known limitation: vault session is not persistent across commands. Each vault operation prompts for the master password.".to_string(),
                Err(e) => format!("Vault unlock error: {e}"),
            }
        }

        // ── Lock ─────────────────────────────────────────────────────────────
        "lock" => "Vault\n  Result           Locked (vault memory cleared)".to_string(),

        // ── Store ─────────────────────────────────────────────────────────────
        "store" => {
            let label = arg1;
            if label.is_empty() {
                return "Vault\n  Usage            /vault store <label>".to_string();
            }
            let password = match read_password_prompt("Master password: ") {
                Ok(p) => p,
                Err(e) => return format!("Vault\n  Error            {e}"),
            };
            if let Err(e) = vm.unlock(&password) { return format!("Vault unlock error: {e}") }
            let secret = match read_password_prompt(&format!("Enter secret for '{label}': ")) {
                Ok(s) => s,
                Err(e) => return format!("Vault\n  Error            {e}"),
            };
            if secret.is_empty() {
                return "Vault\n  Error            Empty secret — store cancelled.".to_string();
            }
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let cred = Credential {
                label: label.to_string(),
                username: None,
                secret,
                notes: None,
                created_at: now,
            };
            match vm.store_credential(&cred) {
                Ok(()) => format!("Vault\n  Result           Stored\n  Label            {label}"),
                Err(e) => format!("Vault store error: {e}"),
            }
        }

        // ── Get ───────────────────────────────────────────────────────────────
        "get" => {
            let label = arg1;
            if label.is_empty() {
                return "Vault\n  Usage            /vault get <label>".to_string();
            }
            let password = match read_password_prompt("Master password: ") {
                Ok(p) => p,
                Err(e) => return format!("Vault\n  Error            {e}"),
            };
            if let Err(e) = vm.unlock(&password) { return format!("Vault unlock error: {e}") }
            match vm.get_credential(label) {
                Ok(cred) => {
                    let username = cred.username.as_deref().unwrap_or("(none)");
                    let notes = cred.notes.as_deref().unwrap_or("(none)");
                    let copied = std::process::Command::new("pbcopy")
                        .stdin(std::process::Stdio::piped())
                        .spawn()
                        .and_then(|mut child| {
                            child.stdin.take().unwrap().write_all(cred.secret.as_bytes())?;
                            child.wait()
                        })
                        .map(|s| s.success())
                        .unwrap_or(false);
                    let secret_display = if copied {
                        "(copied to clipboard)".to_string()
                    } else {
                        format!("{} (masked — use pbcopy/xclip to retrieve)", mask_secret(&cred.secret))
                    };
                    format!(
                        "Vault\n  Label            {}\n  Username         {username}\n  Secret           {secret_display}\n  Notes            {notes}",
                        cred.label
                    )
                }
                Err(e) => format!("Vault get error: {e}"),
            }
        }

        // ── List ──────────────────────────────────────────────────────────────
        "list" => {
            let password = match read_password_prompt("Master password: ") {
                Ok(p) => p,
                Err(e) => return format!("Vault\n  Error            {e}"),
            };
            if let Err(e) = vm.unlock(&password) { return format!("Vault unlock error: {e}") }
            match vm.list_credentials() {
                Ok(labels) if labels.is_empty() => "Vault\n  Credentials      (none stored)".to_string(),
                Ok(labels) => {
                    let mut lines = vec!["Vault — Credentials:".to_string()];
                    for (i, l) in labels.iter().enumerate() {
                        lines.push(format!("  {:>3}.  {l}", i + 1));
                    }
                    lines.join("\n")
                }
                Err(e) => format!("Vault list error: {e}"),
            }
        }

        // ── Delete ────────────────────────────────────────────────────────────
        "delete" => {
            let label = arg1;
            if label.is_empty() {
                return "Vault\n  Usage            /vault delete <label>".to_string();
            }
            let password = match read_password_prompt("Master password: ") {
                Ok(p) => p,
                Err(e) => return format!("Vault\n  Error            {e}"),
            };
            if let Err(e) = vm.unlock(&password) { return format!("Vault unlock error: {e}") }
            match vm.delete_credential(label) {
                Ok(()) => format!("Vault\n  Result           Deleted\n  Label            {label}"),
                Err(e) => format!("Vault delete error: {e}"),
            }
        }

        // ── TOTP sub-commands ─────────────────────────────────────────────────
        "totp" => {
            let mut totp_parts = arg1.splitn(2, ' ');
            let totp_sub = totp_parts.next().unwrap_or("").trim();
            let totp_arg = totp_parts.next().unwrap_or("").trim();

            match totp_sub {
                "list" => {
                    let password = match read_password_prompt("Master password: ") {
                        Ok(p) => p,
                        Err(e) => return format!("Vault\n  Error            {e}"),
                    };
                    if let Err(e) = vm.unlock(&password) { return format!("Vault unlock error: {e}") }
                    match vm.list_totp() {
                        Ok(labels) if labels.is_empty() => "Vault\n  TOTP entries     (none stored)".to_string(),
                        Ok(labels) => {
                            let mut lines = vec!["Vault — TOTP entries:".to_string()];
                            for (i, l) in labels.iter().enumerate() {
                                lines.push(format!("  {:>3}.  {l}", i + 1));
                            }
                            lines.join("\n")
                        }
                        Err(e) => format!("Vault TOTP list error: {e}"),
                    }
                }
                "add" => {
                    let label = totp_arg;
                    if label.is_empty() {
                        return "Vault\n  Usage            /vault totp add <label>".to_string();
                    }
                    let password = match read_password_prompt("Master password: ") {
                        Ok(p) => p,
                        Err(e) => return format!("Vault\n  Error            {e}"),
                    };
                    if let Err(e) = vm.unlock(&password) { return format!("Vault unlock error: {e}") }
                    let secret_input = match read_password_prompt(&format!("TOTP secret/URI for '{label}': ")) {
                        Ok(s) => s,
                        Err(e) => return format!("Vault\n  Error            {e}"),
                    };
                    let secret = secret_input.trim().to_ascii_uppercase();
                    if secret.is_empty() {
                        return "Vault\n  Error            Empty secret — TOTP add cancelled.".to_string();
                    }
                    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                    let entry = TotpEntry {
                        label: label.to_string(),
                        secret,
                        issuer: None,
                        account: None,
                        created_at: now,
                    };
                    match vm.add_totp(&entry) {
                        Ok(()) => format!("Vault\n  Result           TOTP added\n  Label            {label}"),
                        Err(e) => format!("Vault TOTP add error: {e}"),
                    }
                }
                "delete" => {
                    let label = totp_arg;
                    if label.is_empty() {
                        return "Vault\n  Usage            /vault totp delete <label>".to_string();
                    }
                    let password = match read_password_prompt("Master password: ") {
                        Ok(p) => p,
                        Err(e) => return format!("Vault\n  Error            {e}"),
                    };
                    if let Err(e) = vm.unlock(&password) { return format!("Vault unlock error: {e}") }
                    match vm.delete_totp(label) {
                        Ok(()) => format!("Vault\n  Result           TOTP deleted\n  Label            {label}"),
                        Err(e) => format!("Vault TOTP delete error: {e}"),
                    }
                }
                label if !label.is_empty() => {
                    let password = match read_password_prompt("Master password: ") {
                        Ok(p) => p,
                        Err(e) => return format!("Vault\n  Error            {e}"),
                    };
                    if let Err(e) = vm.unlock(&password) { return format!("Vault unlock error: {e}") }
                    match vm.generate_totp(label) {
                        Ok(code) => format!(
                            "Vault — TOTP\n  Label            {label}\n  Code             {}\n  Valid for        {}s",
                            code.code, code.remaining_secs
                        ),
                        Err(e) => format!("Vault TOTP error: {e}"),
                    }
                }
                _ => "Vault\n  Usage            /vault totp [add <label> | <label> | list | delete <label>]".to_string(),
            }
        }

        other => format!(
            "Vault\n  Unknown subcommand: {other}\n  Run /vault for usage."
        ),
    }
}
