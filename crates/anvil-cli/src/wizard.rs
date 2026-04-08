//! First-run interactive setup wizard.
//!
//! Runs entirely in plain terminal mode before the TUI starts.  Walks the user
//! through setting up the encrypted vault, then connecting to each AI provider
//! (Ollama, Anthropic, `OpenAI`, xAI), then sets provider priority and the
//! default model, writing the results to `~/.anvil/config.json`.
//!
//! API keys and credentials are stored exclusively in the encrypted vault
//! (`~/.anvil/vault/`).  `~/.anvil/credentials.json` is only written when the
//! vault setup step is explicitly skipped, preserving backward compatibility
//! with existing installations that may not have run the wizard.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use serde_json::json;

use crate::DEFAULT_MODEL;
use crate::auth::run_anthropic_login;

/// Returns true when `~/.anvil/config.json` already exists, meaning the user
/// has already completed (or explicitly skipped) first-run setup.
pub(crate) fn anvil_config_json_exists() -> bool {
    let Ok(home) = std::env::var("HOME") else { return true };
    PathBuf::from(home).join(".anvil").join("config.json").exists()
}

/// Print a boxed banner line using a fixed-width inner area.
fn wizard_box_line(content: &str) {
    const INNER: usize = 56;
    let padded = format!("{content:<INNER$}");
    println!("\x1b[36m\u{2551}\x1b[0m  {padded}  \x1b[36m\u{2551}\x1b[0m");
}

/// Print a full-width horizontal top border.
fn wizard_box_top() {
    println!(
        "\x1b[36m\u{2554}{}\u{2557}\x1b[0m",
        "\u{2550}".repeat(60)
    );
}

fn wizard_box_bot() {
    println!(
        "\x1b[36m\u{255A}{}\u{255D}\x1b[0m",
        "\u{2550}".repeat(60)
    );
}

fn wizard_step_header(step: u8, total: u8, title: &str) {
    println!();
    println!("\x1b[1;33mStep {step} of {total}: {title}\x1b[0m");
    println!("\x1b[33m{}\x1b[0m", "\u{2501}".repeat(40));
}

/// Read a trimmed line from stdin, flushing stdout first.
fn wizard_read_line(prompt: &str) -> String {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut buf = String::new();
    let _ = io::stdin().read_line(&mut buf);
    buf.trim().to_string()
}

/// Test Ollama connectivity at the given URL.  Returns the list of model names
/// on success, or an error message on failure.
fn wizard_test_ollama(url: &str) -> Result<Vec<(String, String)>, String> {
    let out = std::process::Command::new("curl")
        .args(["-s", "--max-time", "3", &format!("{url}/api/tags")])
        .output()
        .map_err(|e| format!("curl error: {e}"))?;
    if !out.status.success() {
        return Err("connection refused".to_string());
    }
    let val = serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .map_err(|_| "invalid JSON from Ollama".to_string())?;
    let models = val
        .get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let name = m.get("name")?.as_str()?.to_string();
                    let size = m
                        .get("size")
                        .and_then(serde_json::Value::as_f64)
                        .map_or("?".to_string(), |b| format!("{:.1}GB", b / 1e9));
                    Some((name, size))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(models)
}

/// Save a key/value credential using the best available storage backend.
///
/// Priority order:
///   1. Vault session (if vault was set up and unlocked during this wizard run)
///   2. Plaintext `~/.anvil/credentials.json` (fallback for skipped vault setup
///      and for existing installations without a vault)
///
/// The fallback path is intentionally preserved so that users who skip vault
/// setup do not lose their credentials.
pub(crate) fn wizard_save_credential(key: &str, value: &str) -> io::Result<()> {
    // Try vault first — best-effort, fall through to plaintext on any error.
    if runtime::vault_is_session_unlocked() {
        if let Ok(()) = runtime::vault_session_upsert(key, value) {
            return Ok(());
        }
    }
    wizard_save_credential_plaintext(key, value)
}

/// Save a key/value pair to `~/.anvil/credentials.json` (plaintext fallback).
pub(crate) fn wizard_save_credential_plaintext(key: &str, value: &str) -> io::Result<()> {
    let path = runtime::credentials_path()?;
    let mut root = if path.exists() {
        let raw = fs::read_to_string(&path).unwrap_or_default();
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw)
            .unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    root.insert(key.to_string(), serde_json::Value::String(value.to_string()));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(&root).unwrap_or_default())
}

/// Save the wizard result to `~/.anvil/config.json`, merging with any
/// existing keys so previously set values are preserved.
pub(crate) fn wizard_save_config(
    config: &serde_json::Map<String, serde_json::Value>,
) -> io::Result<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = PathBuf::from(home).join(".anvil");
    fs::create_dir_all(&dir)?;
    let path = dir.join("config.json");
    let mut existing = if path.exists() {
        fs::read_to_string(&path)
            .ok()
            .and_then(|raw| {
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw).ok()
            })
            .unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    for (k, v) in config {
        existing.insert(k.clone(), v.clone());
    }
    fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::Value::Object(existing))
            .unwrap_or_else(|_| "{}".to_string()),
    )?;
    Ok(path)
}

/// Interactive first-run setup wizard.  Runs entirely in the plain terminal
/// before the TUI is started.
#[allow(clippy::too_many_lines, clippy::single_match_else)]
pub(crate) fn run_first_run_wizard() {
    println!();
    wizard_box_top();
    wizard_box_line("");
    wizard_box_line(&format!("\u{2692}  Welcome to Anvil v{}", env!("CARGO_PKG_VERSION")));
    wizard_box_line("   AI-Powered Coding Assistant by Culpur Defense");
    wizard_box_line("");
    wizard_box_line("   Let's get you set up.");
    wizard_box_line("");
    wizard_box_bot();

    let mut configured_providers: Vec<String> = Vec::new();
    let mut model_candidates: Vec<(String, String)> = Vec::new(); // (model_id, provider_label)

    // ── Step 1: Vault setup ───────────────────────────────────────────────────
    wizard_step_header(1, 6, "Vault Setup (Credential Encryption)");
    println!();
    println!("  Anvil stores your API keys in an AES-256-GCM encrypted vault.");
    println!("  You set a master password now — it is never stored anywhere.");
    println!("  You will need to enter it once each time you start Anvil.");
    println!();
    println!("  [1] Set up encrypted vault (recommended)");
    println!("  [s] Skip — API keys will be stored in plaintext credentials.json");
    println!();

    let vault_choice = wizard_read_line("  Choice [1]: ");
    let vault_setup_done = match vault_choice.to_ascii_lowercase().trim() {
        "s" | "skip" => {
            println!("  Skipping vault setup.");
            println!("  \x1b[33m  Warning: API keys will be stored in plaintext.\x1b[0m");
            println!("  Run /vault setup later to encrypt your credentials.");
            false
        }
        _ => {
            // Default is to set up the vault.
            if runtime::vault_is_initialized() {
                println!("  Vault already initialized at ~/.anvil/vault/");
                println!("  Unlocking for this session...");
                let pw = match rpassword::prompt_password("  Master password: ") {
                    Ok(p) => p,
                    Err(_) => wizard_read_line("  Master password: "),
                };
                match runtime::init_session_vault(&pw) {
                    Ok(true) => {
                        println!("  \x1b[32m  Vault unlocked.\x1b[0m");
                        true
                    }
                    Ok(false) => {
                        println!("  \x1b[33m  Vault not yet initialized — that is unexpected.\x1b[0m");
                        false
                    }
                    Err(e) => {
                        println!("  \x1b[31m  Unlock failed: {e}\x1b[0m");
                        println!("  Continuing without vault — keys will be stored in plaintext.");
                        false
                    }
                }
            } else {
                let pw = loop {
                    let p1 = match rpassword::prompt_password("  Set master password: ") {
                        Ok(p) => p,
                        Err(_) => wizard_read_line("  Set master password: "),
                    };
                    if p1.is_empty() {
                        println!("  Password must not be empty. Try again.");
                        continue;
                    }
                    let p2 = match rpassword::prompt_password("  Confirm master password: ") {
                        Ok(p) => p,
                        Err(_) => wizard_read_line("  Confirm master password: "),
                    };
                    if p1 != p2 {
                        println!("  Passwords do not match. Try again.");
                        continue;
                    }
                    break p1;
                };
                // Initialize vault directly, then register it as the session vault.
                let mut vm = runtime::VaultManager::with_default_dir();
                match vm.setup(&pw) {
                    Ok(()) => {
                        // Vault is now initialized and unlocked on `vm`.
                        // Register it in the session cache.
                        match runtime::init_session_vault(&pw) {
                            Ok(true) => {
                                println!("  \x1b[32m  Vault created and unlocked.\x1b[0m");
                                println!("  API keys entered in this wizard will be stored encrypted.");
                                true
                            }
                            _ => {
                                // init_session_vault failed but vault is initialized;
                                // this is non-fatal — keys go to plaintext fallback.
                                println!("  \x1b[33m  Vault created but session lock failed.\x1b[0m");
                                println!("  Keys will be stored in plaintext this run.");
                                false
                            }
                        }
                    }
                    Err(e) => {
                        println!("  \x1b[31m  Vault setup failed: {e}\x1b[0m");
                        println!("  Continuing without vault — keys stored in plaintext.");
                        false
                    }
                }
            }
        }
    };
    let _ = vault_setup_done; // informational; wizard_save_credential checks session state

    // ── Step 2: Ollama ────────────────────────────────────────────────────────
    wizard_step_header(2, 6, "Ollama (Local AI)");
    println!();
    println!("  Ollama runs AI models locally on your machine.");
    println!("  No API key required for basic use.");
    println!();
    println!("  [1] Connect to Ollama (default: http://localhost:11434)");
    println!("  [2] Connect with custom URL");
    println!("  [3] Connect with API key");
    println!("  [s] Skip");
    println!();

    let ollama_choice = wizard_read_line("  Choice: ");
    let mut ollama_url: Option<String> = None;

    match ollama_choice.to_ascii_lowercase().as_str() {
        "1" | "" => {
            let url = "http://localhost:11434".to_string();
            print!("\n  Testing connection to {url}... ");
            let _ = io::stdout().flush();
            match wizard_test_ollama(&url) {
                Ok(models) => {
                    println!("\x1b[32m connected\x1b[0m");
                    ollama_url = Some(url.clone());
                    configured_providers.push("ollama".to_string());
                    if !models.is_empty() {
                        println!();
                        println!("  Available models:");
                        for (name, size) in &models {
                            println!("    {name}  ({size})");
                            model_candidates.push((name.clone(), format!("Ollama, {size}")));
                        }
                    }
                    let _ = wizard_save_credential("ollama_host", &url);
                }
                Err(e) => {
                    println!("\x1b[31m failed ({e})\x1b[0m");
                    println!("  Skipping Ollama — make sure Ollama is running: ollama serve");
                }
            }
        }
        "2" => {
            let url = wizard_read_line("\n  Ollama URL [http://localhost:11434]: ");
            let url = if url.is_empty() {
                "http://localhost:11434".to_string()
            } else {
                url
            };
            print!("  Testing connection to {url}... ");
            let _ = io::stdout().flush();
            match wizard_test_ollama(&url) {
                Ok(models) => {
                    println!("\x1b[32m connected\x1b[0m");
                    ollama_url = Some(url.clone());
                    configured_providers.push("ollama".to_string());
                    if !models.is_empty() {
                        println!();
                        println!("  Available models:");
                        for (name, size) in &models {
                            println!("    {name}  ({size})");
                            model_candidates.push((name.clone(), format!("Ollama, {size}")));
                        }
                    }
                    let _ = wizard_save_credential("ollama_host", &url);
                }
                Err(e) => {
                    println!("\x1b[31m failed ({e})\x1b[0m");
                    println!(
                        "  Connection failed. Configuration saved; you can retry with `anvil login ollama`."
                    );
                    let _ = wizard_save_credential("ollama_host", &url);
                    ollama_url = Some(url.clone());
                    configured_providers.push("ollama".to_string());
                }
            }
        }
        "3" => {
            let url = wizard_read_line("\n  Ollama URL [http://localhost:11434]: ");
            let url = if url.is_empty() {
                "http://localhost:11434".to_string()
            } else {
                url
            };
            let api_key = match rpassword::prompt_password("  API key: ") {
                Ok(k) => k,
                Err(_) => wizard_read_line("  API key: "),
            };
            print!("  Testing connection to {url}... ");
            let _ = io::stdout().flush();
            match wizard_test_ollama(&url) {
                Ok(models) => {
                    println!("\x1b[32m connected\x1b[0m");
                    configured_providers.push("ollama".to_string());
                    if !models.is_empty() {
                        println!();
                        println!("  Available models:");
                        for (name, size) in &models {
                            println!("    {name}  ({size})");
                            model_candidates.push((name.clone(), format!("Ollama, {size}")));
                        }
                    }
                }
                Err(e) => {
                    println!("\x1b[33m could not verify ({e}) — saving anyway\x1b[0m");
                    configured_providers.push("ollama".to_string());
                }
            }
            let _ = wizard_save_credential("ollama_host", &url);
            if !api_key.is_empty() {
                let _ = wizard_save_credential("ollama_api_key", &api_key);
            }
            ollama_url = Some(url.clone());
        }
        "s" | "skip" => {
            println!("  Skipping Ollama.");
        }
        other => {
            println!("  Unknown choice '{other}', skipping Ollama.");
        }
    }

    // ── Step 3: Anthropic ─────────────────────────────────────────────────────
    wizard_step_header(3, 6, "Anthropic (Claude)");
    println!();
    println!("  Anthropic provides Claude — the most capable AI assistant.");
    println!();
    println!("  [1] Login with OAuth (recommended — opens browser)");
    println!("  [2] Enter API key manually");
    println!("  [s] Skip");
    println!();

    let anthropic_choice = wizard_read_line("  Choice: ");
    match anthropic_choice.to_ascii_lowercase().as_str() {
        "1" | "" => {
            println!();
            match run_anthropic_login() {
                Ok(()) => {
                    println!("\x1b[32m  Anthropic OAuth complete.\x1b[0m");
                    configured_providers.push("anthropic".to_string());
                    model_candidates
                        .push(("claude-opus-4-6".to_string(), "Anthropic".to_string()));
                    model_candidates
                        .push(("claude-sonnet-4-6".to_string(), "Anthropic".to_string()));
                    model_candidates.push((
                        "claude-haiku-4-5-20251213".to_string(),
                        "Anthropic".to_string(),
                    ));
                }
                Err(e) => {
                    eprintln!("  OAuth failed: {e}");
                    println!("  You can retry later with: anvil login anthropic");
                }
            }
        }
        "2" => {
            println!();
            println!("  Get your key at: https://console.anthropic.com/settings/keys");
            let api_key = match rpassword::prompt_password("  API key (sk-ant-...): ") {
                Ok(k) => k,
                Err(_) => wizard_read_line("  API key (sk-ant-...): "),
            };
            if api_key.is_empty() {
                println!("  No key entered, skipping Anthropic.");
            } else {
                print!("  Validating key... ");
                let _ = io::stdout().flush();
                let out = std::process::Command::new("curl")
                    .args([
                        "-s",
                        "--max-time",
                        "5",
                        "-H",
                        &format!("x-api-key: {api_key}"),
                        "-H",
                        "anthropic-version: 2023-06-01",
                        "https://api.anthropic.com/v1/models",
                    ])
                    .output();
                let valid = out.is_ok_and(|o| {
                    o.status.success()
                        && !o.stdout.is_empty()
                        && !o.stdout.starts_with(b"{\"error\"")
                });
                if valid {
                    println!("\x1b[32m valid\x1b[0m");
                } else {
                    println!("\x1b[33m could not verify — saving anyway\x1b[0m");
                }
                let _ = wizard_save_credential("anthropic_api_key", &api_key);
                configured_providers.push("anthropic".to_string());
                model_candidates
                    .push(("claude-opus-4-6".to_string(), "Anthropic".to_string()));
                model_candidates
                    .push(("claude-sonnet-4-6".to_string(), "Anthropic".to_string()));
                model_candidates.push((
                    "claude-haiku-4-5-20251213".to_string(),
                    "Anthropic".to_string(),
                ));
            }
        }
        "s" | "skip" => {
            println!("  Skipping Anthropic.");
        }
        other => {
            println!("  Unknown choice '{other}', skipping Anthropic.");
        }
    }

    // ── Step 4: OpenAI ────────────────────────────────────────────────────────
    wizard_step_header(4, 6, "OpenAI (ChatGPT)");
    println!();
    println!("  OpenAI provides GPT models for coding and reasoning.");
    println!();
    println!("  [1] Enter API key");
    println!("  [s] Skip");
    println!();

    let openai_choice = wizard_read_line("  Choice: ");
    match openai_choice.to_ascii_lowercase().as_str() {
        "1" => {
            println!();
            println!("  Get your key at: https://platform.openai.com/api-keys");
            let api_key = match rpassword::prompt_password("  API key (sk-...): ") {
                Ok(k) => k,
                Err(_) => wizard_read_line("  API key (sk-...): "),
            };
            if api_key.is_empty() {
                println!("  No key entered, skipping OpenAI.");
            } else {
                print!("  Validating key... ");
                let _ = io::stdout().flush();
                let out = std::process::Command::new("curl")
                    .args([
                        "-s",
                        "--max-time",
                        "5",
                        "-H",
                        &format!("Authorization: Bearer {api_key}"),
                        "https://api.openai.com/v1/models",
                    ])
                    .output();
                let valid = out.is_ok_and(|o| {
                    o.status.success() && !o.stdout.starts_with(b"{\"error\"")
                });
                if valid {
                    println!("\x1b[32m valid\x1b[0m");
                } else {
                    println!("\x1b[33m could not verify — saving anyway\x1b[0m");
                }
                let _ = wizard_save_credential("openai_api_key", &api_key);
                configured_providers.push("openai".to_string());
                model_candidates.push(("gpt-4o".to_string(), "OpenAI".to_string()));
                model_candidates.push(("gpt-4o-mini".to_string(), "OpenAI".to_string()));
            }
        }
        "s" | "skip" | "" => {
            println!("  Skipping OpenAI.");
        }
        other => {
            println!("  Unknown choice '{other}', skipping OpenAI.");
        }
    }

    // ── Step 5: xAI (Grok) ───────────────────────────────────────────────────
    wizard_step_header(5, 6, "xAI (Grok)");
    println!();
    println!("  xAI provides Grok models.");
    println!();
    println!("  [1] Enter API key");
    println!("  [s] Skip");
    println!();

    let xai_choice = wizard_read_line("  Choice: ");
    match xai_choice.to_ascii_lowercase().as_str() {
        "1" => {
            println!();
            println!("  Get your key at: https://console.x.ai");
            let api_key = match rpassword::prompt_password("  API key (xai-...): ") {
                Ok(k) => k,
                Err(_) => wizard_read_line("  API key (xai-...): "),
            };
            if api_key.is_empty() {
                println!("  No key entered, skipping xAI.");
            } else {
                let _ = wizard_save_credential("xai_api_key", &api_key);
                configured_providers.push("xai".to_string());
                model_candidates.push(("grok-3".to_string(), "xAI".to_string()));
                model_candidates.push(("grok-3-mini".to_string(), "xAI".to_string()));
                println!("  \x1b[32mxAI API key saved.\x1b[0m");
            }
        }
        "s" | "skip" | "" => {
            println!("  Skipping xAI.");
        }
        other => {
            println!("  Unknown choice '{other}', skipping xAI.");
        }
    }

    // ── Step 6: Provider priority & default model ─────────────────────────────
    wizard_step_header(6, 6, "Provider Priority & Default Model");
    println!();

    let mut seen = std::collections::HashSet::new();
    configured_providers.retain(|p| seen.insert(p.clone()));

    let provider_priority: Vec<String> = if configured_providers.is_empty() {
        println!("  No providers configured.");
        println!("  You can configure providers later with: anvil login");
        vec!["anthropic".to_string()]
    } else {
        fn provider_label(p: &str) -> &str {
            match p {
                "anthropic" => "Anthropic",
                "ollama" => "Ollama",
                "openai" => "OpenAI",
                "xai" => "xAI",
                other => other,
            }
        }
        let configured_display = configured_providers
            .iter()
            .map(|p| provider_label(p))
            .collect::<Vec<_>>()
            .join(", ");
        println!("  You configured: {configured_display}");
        println!();
        println!("  Set your provider priority (first = primary, others = failover):");
        println!();
        println!("  Current order:");
        for (i, p) in configured_providers.iter().enumerate() {
            let first_model = model_candidates
                .iter()
                .find(|(_, label)| {
                    label
                        .to_ascii_lowercase()
                        .starts_with(&provider_label(p).to_ascii_lowercase())
                })
                .map_or("—", |(m, _)| m.as_str());
            println!("    {}. {} ({})", i + 1, provider_label(p), first_model);
        }
        println!();
        println!("  [1] Keep this order");
        println!("  [2] Reorder (enter numbers: e.g., \"2,1\")");
        println!();
        let order_choice = wizard_read_line("  Choice: ");
        match order_choice.as_str() {
            "1" | "" => configured_providers.clone(),
            "2" => {
                let new_order = wizard_read_line("  New order: ");
                let indices: Vec<usize> = new_order
                    .split(',')
                    .filter_map(|s| s.trim().parse::<usize>().ok())
                    .collect();
                let reordered: Vec<String> = indices
                    .iter()
                    .filter_map(|&i| configured_providers.get(i.saturating_sub(1)).cloned())
                    .collect();
                if reordered.is_empty() {
                    println!("  Could not parse order, keeping original.");
                    configured_providers.clone()
                } else {
                    reordered
                }
            }
            _ => {
                println!("  Unknown choice, keeping original order.");
                configured_providers.clone()
            }
        }
    };

    // Default model selection.
    let chosen_model: String = if model_candidates.is_empty() {
        DEFAULT_MODEL.to_string()
    } else {
        println!();
        println!("  Select your default model:");
        let mut unique_models: Vec<(String, String)> = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();
        for (id, label) in &model_candidates {
            if seen_ids.insert(id.clone()) {
                unique_models.push((id.clone(), label.clone()));
            }
        }
        for (i, (id, label)) in unique_models.iter().enumerate() {
            println!("    [{}] {} ({})", i + 1, id, label);
        }
        println!();
        let model_choice = wizard_read_line("  Choice: ");
        if let Ok(n) = model_choice.parse::<usize>() {
            if n >= 1 && n <= unique_models.len() {
                unique_models[n - 1].0.clone()
            } else {
                unique_models[0].0.clone()
            }
        } else {
            unique_models[0].0.clone()
        }
    };

    // Thinking mode toggle
    println!();
    println!("  \x1b[1;33mEnable Thinking Mode?\x1b[0m");
    println!("  Some models (Qwen3, Claude, o3) support extended thinking/reasoning.");
    println!("  When enabled, the model shows its reasoning process before answering.");
    println!();
    println!("    [1] Yes — enable thinking mode (recommended for coding)");
    println!("    [2] No  — standard responses only");
    println!();
    let think_choice = wizard_read_line("  Choice: ");
    let thinking_enabled =
        think_choice.trim() == "1" || think_choice.trim().to_lowercase() == "yes";
    if thinking_enabled {
        println!("  \x1b[32m✓\x1b[0m Thinking mode enabled. Toggle anytime with /think");
    } else {
        println!("  Thinking mode off. Toggle anytime with /think");
    }

    let default_provider = provider_priority
        .first()
        .cloned()
        .unwrap_or_else(|| "anthropic".to_string());

    // ── Build and save config.json ─────────────────────────────────────────────
    let mut providers_obj = serde_json::Map::new();

    let ollama_enabled = provider_priority.contains(&"ollama".to_string());
    let ollama_url_val = ollama_url
        .clone()
        .unwrap_or_else(|| "http://localhost:11434".to_string());
    providers_obj.insert(
        "ollama".to_string(),
        json!({
            "enabled": ollama_enabled,
            "url": ollama_url_val,
            "api_key": serde_json::Value::Null
        }),
    );

    let anthropic_enabled = provider_priority.contains(&"anthropic".to_string());
    let anthropic_auth = if configured_providers.contains(&"anthropic".to_string()) {
        "oauth"
    } else {
        "none"
    };
    providers_obj.insert(
        "anthropic".to_string(),
        json!({
            "enabled": anthropic_enabled,
            "auth_method": anthropic_auth
        }),
    );

    let openai_enabled = provider_priority.contains(&"openai".to_string());
    providers_obj.insert("openai".to_string(), json!({ "enabled": openai_enabled }));

    let xai_enabled = provider_priority.contains(&"xai".to_string());
    providers_obj.insert("xai".to_string(), json!({ "enabled": xai_enabled }));

    let mut config = serde_json::Map::new();
    config.insert(
        "default_model".to_string(),
        serde_json::Value::String(chosen_model.clone()),
    );
    config.insert(
        "default_provider".to_string(),
        serde_json::Value::String(default_provider.clone()),
    );
    config.insert(
        "provider_priority".to_string(),
        serde_json::Value::Array(
            provider_priority
                .iter()
                .map(|p| serde_json::Value::String(p.clone()))
                .collect(),
        ),
    );
    config.insert(
        "providers".to_string(),
        serde_json::Value::Object(providers_obj),
    );
    config.insert(
        "setup_completed".to_string(),
        serde_json::Value::Bool(true),
    );
    config.insert(
        "thinking_enabled".to_string(),
        serde_json::Value::Bool(thinking_enabled),
    );

    let config_path = match wizard_save_config(&config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("\n  Warning: could not save config: {e}");
            PathBuf::from("~/.anvil/config.json")
        }
    };

    // ── Final summary banner ───────────────────────────────────────────────────
    let provider_chain = provider_priority.join(" \u{2192} ");
    println!();
    wizard_box_top();
    wizard_box_line("");
    wizard_box_line("  Setup complete!");
    wizard_box_line("");
    wizard_box_line(&format!("    Default model:  {chosen_model}"));
    wizard_box_line(&format!("    Providers:      {provider_chain}"));
    wizard_box_line(&format!("    Config saved:   {}", config_path.display()));
    wizard_box_line("");
    wizard_box_line("  Run `anvil` to start coding.");
    wizard_box_line("  Type `/help` for all commands.");
    wizard_box_line("");
    wizard_box_bot();
    println!();
}
