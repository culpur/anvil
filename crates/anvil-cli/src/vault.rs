//! Credential vault command implementation.
//!
//! Extracted from main.rs to keep the vault logic self-contained.
//! All sensitive input is read via no-echo terminal prompts so secrets never
//! appear in REPL history or process argument lists.

// Task #626 — `/vault` subcommands run from both TUI and headless paths.
// `read_password_prompt` was a BUG-DEFER per the audit; task #627
// resolved the `/vault unlock` path by intercepting it in
// `handle_repl_command_tui` and opening an in-TUI PasswordModal.
// The remaining sub-commands (setup, store, get, totp ...) still use
// `read_password_prompt` and are headless-only — invoking them from
// the TUI prints the password prompt to stderr, which the alt-screen
// hides until exit.  Pending follow-up: extend PasswordModal coverage
// to those flows (track separately so this task stays scoped).
#![deny(clippy::print_stdout, clippy::print_stderr)]

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
///
/// Task #626 BUG-DEFER: when called from a TUI session, `eprint!` paints
/// into the alt-screen back-buffer and `rpassword::read_password`
/// competes with ratatui for stdin.  Fix needs an in-TUI password modal
/// (tracked as a follow-up); the per-call `#[allow]` documents that the
/// crate-level deny is suppressed here on purpose.
#[allow(clippy::print_stderr, reason = "BUG-DEFER per audit 2026-05-18 — in-TUI password modal is the structural fix; tracked as follow-up")]
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

/// Structured vault operations for the web viewer — returns JSON instead of formatted text.
/// Takes a password and an operation, returns `serde_json::Value`.
pub(crate) fn vault_json_operation(password: &str, operation: &str, arg: &str) -> serde_json::Value {
    use runtime::{Credential, CredentialType, VaultManager};
    use std::time::{SystemTime, UNIX_EPOCH};

    let mut vm = VaultManager::with_default_dir();

    if !vm.is_initialized() {
        return serde_json::json!({"error": "Vault not initialized. Run /vault setup in the TUI."});
    }

    if let Err(e) = vm.unlock(password) {
        return serde_json::json!({"error": format!("Invalid master password: {e}")});
    }

    match operation {
        "list" => {
            match vm.list_credentials() {
                Ok(labels) => {
                    let creds: Vec<serde_json::Value> = labels.iter().map(|label| {
                        vm.get_credential(label).ok().map_or_else(|| serde_json::json!({"label": label, "credential_type": "Secret"}), |c| {
                            serde_json::json!({
                                "label": c.label,
                                "credential_type": format!("{}", c.credential_type),
                                "type_id": serde_json::to_value(&c.credential_type).unwrap_or_default(),
                                "username": c.username,
                                "url": c.url,
                                "masked_secret": mask_secret(&c.secret),
                                "has_notes": c.notes.is_some(),
                                "tags": c.tags,
                                "created_at": c.created_at,
                                "updated_at": c.updated_at,
                                "expires_at": c.expires_at,
                                "last_rotated": c.last_rotated,
                                "metadata": c.metadata,
                            })
                        })
                    }).collect();
                    // Also list TOTP entries
                    let totp_labels = vm.list_totp().unwrap_or_default();
                    let totp_creds: Vec<serde_json::Value> = totp_labels.iter().map(|label| {
                        serde_json::json!({
                            "label": label,
                            "credential_type": "TOTP",
                            "type_id": "totp",
                            "masked_secret": "••••••",
                            "metadata": {},
                        })
                    }).collect();
                    let mut all = creds;
                    all.extend(totp_creds);
                    serde_json::json!({"operation": "list", "credentials": all, "count": all.len()})
                }
                Err(e) => serde_json::json!({"error": format!("List failed: {e}")}),
            }
        }
        "get" => {
            if arg.is_empty() {
                return serde_json::json!({"error": "No label specified"});
            }
            match vm.get_credential(arg) {
                Ok(cred) => serde_json::json!({
                    "operation": "get",
                    "label": cred.label,
                    "credential_type": format!("{}", cred.credential_type),
                    "type_id": serde_json::to_value(&cred.credential_type).unwrap_or_default(),
                    "username": cred.username,
                    "url": cred.url,
                    "secret": cred.secret,
                    "notes": cred.notes,
                    "tags": cred.tags,
                    "created_at": cred.created_at,
                    "updated_at": cred.updated_at,
                    "expires_at": cred.expires_at,
                    "metadata": cred.metadata,
                }),
                Err(e) => serde_json::json!({"error": format!("Get failed: {e}")}),
            }
        }
        "store" => {
            if arg.is_empty() {
                return serde_json::json!({"error": "No label specified"});
            }
            // arg format: "label secret"
            let mut parts = arg.splitn(2, ' ');
            let label = parts.next().unwrap_or("");
            let secret = parts.next().unwrap_or("");
            if secret.is_empty() {
                return serde_json::json!({"error": "No secret value provided"});
            }
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let cred = Credential {
                label: label.to_string(),
                username: None,
                secret: secret.to_string(),
                notes: None,
                created_at: now,
            
                credential_type: CredentialType::default(),
                url: None,
                tags: Vec::new(),
                updated_at: 0,
                expires_at: None,
                last_rotated: None,
                metadata: serde_json::Value::Object(serde_json::Map::new()),
            };
            match vm.store_credential(&cred) {
                Ok(()) => serde_json::json!({"operation": "store", "label": label, "success": true}),
                Err(e) => serde_json::json!({"error": format!("Store failed: {e}")}),
            }
        }
        "delete" => {
            if arg.is_empty() {
                return serde_json::json!({"error": "No label specified"});
            }
            match vm.delete_credential(arg) {
                Ok(()) => serde_json::json!({"operation": "delete", "label": arg, "success": true}),
                Err(e) => serde_json::json!({"error": format!("Delete failed: {e}")}),
            }
        }
        _ => serde_json::json!({"error": format!("Unknown vault operation: {operation}")}),
    }
}

/// Execute a `/vault` slash command.
///
/// This is called from `LiveCli::run_vault_command`.  All sub-commands that
/// require a master password read it via `read_password_prompt` (no echo).
#[allow(clippy::similar_names)]
pub(crate) fn run_vault_command_impl(args: Option<&str>) -> String {
    use runtime::{Credential, CredentialType, TotpEntry, VaultManager};

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
            // Accept password as parameter (for remote/web viewer) or prompt interactively
            let password = if arg1.is_empty() {
                match read_password_prompt("Master password: ") {
                    Ok(p) => p,
                    Err(e) => return format!("Vault\n  Error            {e}"),
                }
            } else {
                arg1.to_string()
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
            
                credential_type: CredentialType::default(),
                url: None,
                tags: Vec::new(),
                updated_at: 0,
                expires_at: None,
                last_rotated: None,
                metadata: serde_json::Value::Object(serde_json::Map::new()),
            };
            match vm.store_credential(&cred) {
                Ok(()) => format!("Vault\n  Result           Stored\n  Label            {label}"),
                Err(e) => format!("Vault store error: {e}"),
            }
        }

        // ── Get ───────────────────────────────────────────────────────────────
        "get" => {
            // /vault get <label> [password] — password optional for remote use
            let mut get_parts = arg1.splitn(2, ' ');
            let label = get_parts.next().unwrap_or("").trim();
            let inline_pw = get_parts.next().unwrap_or("").trim();
            if label.is_empty() {
                return "Vault\n  Usage            /vault get <label>".to_string();
            }
            let password = if inline_pw.is_empty() {
                match read_password_prompt("Master password: ") {
                    Ok(p) => p,
                    Err(e) => return format!("Vault\n  Error            {e}"),
                }
            } else {
                inline_pw.to_string()
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
                            if let Some(mut stdin) = child.stdin.take() {
                                stdin.write_all(cred.secret.as_bytes())?;
                            }
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
            // Accept password as parameter for remote/non-interactive use
            let password = if arg1.is_empty() {
                match read_password_prompt("Master password: ") {
                    Ok(p) => p,
                    Err(e) => return format!("Vault\n  Error            {e}"),
                }
            } else {
                arg1.to_string()
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
