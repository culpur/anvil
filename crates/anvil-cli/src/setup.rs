//! `anvil --setup` / `anvil setup` — first-run installation wizard.
//!
//! Distinct from `wizard.rs` (which is the interactive provider-configuration
//! wizard used inside the running REPL).  This module handles the post-install
//! flow triggered by the installer scripts:
//!
//! 1. Create `~/.anvil/` directory structure
//! 2. Prompt the user to pick an AI provider
//! 3. Guide API-key entry (written to vault / config.json)
//! 4. Optionally prompt about Ollama
//! 5. Install shell completions to the appropriate location
//! 6. Print a success summary

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;

use serde_json::json;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn prompt(msg: &str) -> String {
    print!("{msg}");
    let _ = io::stdout().flush();
    let mut buf = String::new();
    let _ = io::stdin().read_line(&mut buf);
    buf.trim().to_string()
}

fn info(msg: &str) {
    println!("  \x1b[36m>\x1b[0m {msg}");
}

fn success(msg: &str) {
    println!("  \x1b[32m\u{2714}\x1b[0m {msg}");
}

fn warn(msg: &str) {
    println!("  \x1b[33m\u{26A0}\x1b[0m {msg}");
}

fn anvil_home() -> PathBuf {
    if let Ok(h) = std::env::var("ANVIL_CONFIG_HOME") {
        return PathBuf::from(h);
    }
    dirs_next::home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        .join(".anvil")
}

// ── Provider selection ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Provider {
    Anthropic,
    OpenAi,
    Google,
    XAi,
    Ollama,
}

impl Provider {
    fn name(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Google => "google",
            Self::XAi => "xai",
            Self::Ollama => "ollama",
        }
    }

    fn display(&self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic (Claude)",
            Self::OpenAi => "OpenAI (GPT-4, o-series)",
            Self::Google => "Google (Gemini)",
            Self::XAi => "xAI (Grok)",
            Self::Ollama => "Ollama (local models)",
        }
    }

    fn key_env(&self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("ANTHROPIC_API_KEY"),
            Self::OpenAi => Some("OPENAI_API_KEY"),
            Self::Google => Some("GOOGLE_API_KEY"),
            Self::XAi => Some("XAI_API_KEY"),
            Self::Ollama => None,
        }
    }

    fn docs_url(&self) -> &'static str {
        match self {
            Self::Anthropic => "https://console.anthropic.com/settings/keys",
            Self::OpenAi => "https://platform.openai.com/api-keys",
            Self::Google => "https://aistudio.google.com/app/apikey",
            Self::XAi => "https://console.x.ai/",
            Self::Ollama => "https://ollama.com",
        }
    }
}

fn parse_provider(s: &str) -> Option<Provider> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "anthropic" | "claude" => Some(Provider::Anthropic),
        "2" | "openai" => Some(Provider::OpenAi),
        "3" | "google" | "gemini" => Some(Provider::Google),
        "4" | "xai" | "grok" => Some(Provider::XAi),
        "5" | "ollama" => Some(Provider::Ollama),
        _ => None,
    }
}

// ── Directory bootstrap ───────────────────────────────────────────────────────

fn create_directory_structure(home: &PathBuf) -> Result<(), String> {
    for subdir in &["vault", "sessions", "logs"] {
        let dir = home.join(subdir);
        fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    Ok(())
}

// ── Config writing ────────────────────────────────────────────────────────────

fn write_provider_config(
    home: &PathBuf,
    provider: &Provider,
    api_key: &str,
    ollama_url: Option<&str>,
) -> Result<(), String> {
    let config_path = home.join("config.json");

    // Load existing or start fresh
    let mut val: serde_json::Value = if config_path.exists() {
        let data = fs::read_to_string(&config_path)
            .map_err(|e| format!("cannot read config.json: {e}"))?;
        serde_json::from_str(&data).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    // Ensure providers object exists
    if val.get("providers").is_none() {
        val["providers"] = json!({});
    }

    let p = provider.name();
    match provider {
        Provider::Ollama => {
            val["providers"][p] = json!({
                "url": ollama_url.unwrap_or("http://localhost:11434"),
            });
        }
        _ => {
            if !api_key.is_empty() {
                val["providers"][p] = json!({ "api_key": api_key });
            }
        }
    }

    // Set default_model if not already set
    if val.get("default_model").is_none() {
        let default_model = match provider {
            Provider::Anthropic => "claude-opus-4-6",
            Provider::OpenAi => "gpt-4o",
            Provider::Google => "gemini-1.5-pro",
            Provider::XAi => "grok-3",
            Provider::Ollama => "qwen2.5-coder:7b",
        };
        val["default_model"] = json!(default_model);
    }

    let json_str = serde_json::to_string_pretty(&val)
        .map_err(|e| format!("cannot serialize config: {e}"))?;
    fs::write(&config_path, json_str)
        .map_err(|e| format!("cannot write config.json: {e}"))?;

    // Tighten permissions: config.json may hold API keys in plaintext until
    // the user migrates them into the encrypted vault. Mode 0600 means only
    // the owning user can read. No-op on non-Unix (NTFS ACLs handle this).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&config_path, perms)
            .map_err(|e| format!("cannot set config.json permissions: {e}"))?;
    }

    Ok(())
}

// ── Shell completions installation ────────────────────────────────────────────

/// Detect shells that the user likely has configured.
fn detect_shells() -> Vec<&'static str> {
    let mut shells = Vec::new();
    if let Ok(shell_env) = std::env::var("SHELL") {
        if shell_env.contains("zsh") {
            shells.push("zsh");
        } else if shell_env.contains("bash") {
            shells.push("bash");
        } else if shell_env.contains("fish") {
            shells.push("fish");
        }
    }
    // Always try all three if we can't tell
    if shells.is_empty() {
        shells.push("bash");
        shells.push("zsh");
    }
    shells
}

fn install_completions(home: &PathBuf) -> Vec<String> {
    let mut installed = Vec::new();

    // Locate completion files next to the binary (installed by the installer)
    // or in a known share location.
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from));

    let completion_sources: Vec<PathBuf> = [
        exe_dir
            .as_ref()
            .map(|d| d.join("../share/anvil/completions")),
        Some(PathBuf::from("/usr/local/share/anvil/completions")),
        Some(PathBuf::from("/opt/homebrew/share/anvil/completions")),
    ]
    .into_iter()
    .flatten()
    .collect();

    for shell in detect_shells() {
        let filename = format!("anvil.{shell}");
        let src = completion_sources.iter().find_map(|d| {
            let p = d.join(&filename);
            if p.exists() { Some(p) } else { None }
        });

        let Some(src) = src else {
            continue;
        };

        let dest_dir: Option<PathBuf> = match shell {
            "bash" => {
                // ~/.local/share/bash-completion/completions/
                home.parent()
                    .map(|h| h.join(".local/share/bash-completion/completions"))
            }
            "zsh" => {
                // ~/.zsh/completions/ or ~/.zfunc/
                let zfunc = home.parent().map(|h| h.join(".zfunc"));
                if let Some(ref z) = zfunc {
                    if z.exists() {
                        zfunc
                    } else {
                        home.parent().map(|h| h.join(".zsh/completions"))
                    }
                } else {
                    None
                }
            }
            "fish" => home
                .parent()
                .map(|h| h.join(".config/fish/completions")),
            _ => None,
        };

        if let Some(dest_dir) = dest_dir {
            let _ = fs::create_dir_all(&dest_dir);
            let dest = dest_dir.join(&filename);
            if fs::copy(&src, &dest).is_ok() {
                installed.push(dest.display().to_string());
            }
        }
    }

    installed
}

// ── Ollama optional install ───────────────────────────────────────────────────

fn check_ollama_installed() -> bool {
    Command::new("ollama")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn prompt_ollama_install() -> bool {
    println!();
    println!("  Ollama lets you run AI models locally (no API key required).");
    let answer = prompt("  Install Ollama now? [y/N] ");
    matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Curated Ollama model menu.
///
/// General-purpose models users can pick from after Ollama is installed.
/// We deliberately exclude DeepSeek and Kimi. Origins are labeled so the
/// user can make an informed choice about which weights to pull.
const GENERAL_MODELS: &[(&str, &str, &str)] = &[
    // (ollama-tag, approximate-size, label)
    ("llama3.1:8b",      "~5 GB",  "Llama 3.1 8B (Meta) — recommended default"),
    ("qwen3:8b",         "~5 GB",  "Qwen 3 8B (Alibaba)"),
    ("mistral-nemo:12b", "~7 GB",  "Mistral Nemo 12B (Mistral AI)"),
    ("gemma3:4b",        "~3 GB",  "Gemma 3 4B (Google) — low spec"),
    ("phi4:14b",         "~9 GB",  "Phi 4 14B (Microsoft) — reasoning-focused"),
    ("qwen3:14b",        "~9 GB",  "Qwen 3 14B (Alibaba) — larger variant"),
    ("llama3.3:70b",     "~43 GB", "Llama 3.3 70B (Meta) — power user (64+ GB RAM)"),
];

/// Coding-specialized models. Same origin policy as general models.
const CODING_MODELS: &[(&str, &str, &str)] = &[
    ("qwen2.5-coder:7b",  "~5 GB",  "Qwen 2.5-Coder 7B (Alibaba) — recommended default"),
    ("codellama:13b",     "~7 GB",  "Code Llama 13B (Meta)"),
    ("codestral:22b",     "~13 GB", "Codestral 22B (Mistral AI)"),
    ("qwen2.5-coder:14b", "~9 GB",  "Qwen 2.5-Coder 14B (Alibaba) — larger"),
    ("codellama:7b",      "~4 GB",  "Code Llama 7B (Meta) — smallest footprint"),
    ("qwen3-coder:30b",   "~18 GB", "Qwen 3-Coder 30B (Alibaba) — heaviest"),
];

/// Present the model menu and pull each model the user confirms.
/// Zero network activity happens without an explicit "y" answer.
fn prompt_model_menu<'a>(
    kind: &str,
    choices: &'a [(&'static str, &'static str, &'static str)],
) -> Option<&'a str> {
    println!();
    println!("  Pick a {kind} model (or skip):");
    for (idx, (_tag, size, label)) in choices.iter().enumerate() {
        println!("    {}) {label:<55} [{size}]", idx + 1);
    }
    println!("    {}) Skip", choices.len() + 1);
    let answer = prompt(&format!("  Your choice [1-{}]: ", choices.len() + 1));
    let pick: Option<usize> = answer.trim().parse().ok();
    match pick {
        Some(n) if n >= 1 && n <= choices.len() => Some(choices[n - 1].0),
        _ => None,
    }
}

fn pull_model(tag: &str) -> bool {
    let confirm = prompt(&format!(
        "  Download {tag} now? (Ollama will pull several GB) [y/N] "
    ));
    if !matches!(confirm.to_ascii_lowercase().as_str(), "y" | "yes") {
        info(&format!("Skipped {tag}. You can pull it later with `ollama pull {tag}`"));
        return false;
    }
    info(&format!("Pulling {tag} (this may take a few minutes)..."));
    let pull = Command::new("ollama").args(["pull", tag]).status();
    match pull {
        Ok(status) if status.success() => {
            success(&format!("{tag} ready."));
            true
        }
        _ => {
            warn(&format!(
                "Could not pull {tag} — you can retry later with `ollama pull {tag}`"
            ));
            false
        }
    }
}

fn install_ollama() -> Result<(), String> {
    // Double-prompt: a plain "install Ollama?" isn't enough — we're about to
    // pipe a remote shell script into sh. Surface the exact URL and what will
    // happen, then re-confirm.
    println!();
    println!("  \x1b[1;33mAbout to run:\x1b[0m");
    println!("    sh -c \"curl -fsSL https://ollama.com/install.sh | sh\"");
    println!();
    println!("  This downloads and executes the official Ollama install script");
    println!("  from ollama.com. It may require sudo.");
    let confirm = prompt("  Proceed with installing Ollama? [y/N] ");
    if !matches!(confirm.to_ascii_lowercase().as_str(), "y" | "yes") {
        info("Ollama install cancelled. You can install it later from https://ollama.com");
        return Err("Ollama install cancelled by user".to_string());
    }

    // Step 1: official Ollama installer
    let out = Command::new("sh")
        .args(["-c", "curl -fsSL https://ollama.com/install.sh | sh"])
        .status()
        .map_err(|e| format!("failed to run Ollama installer: {e}"))?;

    if !out.success() {
        return Err("Ollama installer exited with a non-zero status".to_string());
    }

    // Step 2: offer the model menus. Each pull requires explicit confirmation.
    println!();
    println!("  \x1b[1mOllama is installed. Choose which models to download.\x1b[0m");
    println!("  DeepSeek and Kimi are intentionally excluded.");

    if let Some(tag) = prompt_model_menu("general-purpose", GENERAL_MODELS) {
        pull_model(tag);
    }

    if let Some(tag) = prompt_model_menu("coding-specialized", CODING_MODELS) {
        pull_model(tag);
    }

    Ok(())
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the first-run setup wizard for a fresh install.
/// Called by `anvil --setup` / the installer scripts.
pub(crate) fn run_setup_wizard() {
    println!();
    println!("\x1b[1;36m\u{2554}{}\u{2557}\x1b[0m", "\u{2550}".repeat(58));
    println!(
        "\x1b[1;36m\u{2551}\x1b[0m  \x1b[1mAnvil v{} — First-run setup\x1b[0m{}  \x1b[1;36m\u{2551}\x1b[0m",
        crate::VERSION,
        " ".repeat(58usize.saturating_sub(28 + crate::VERSION.len()))
    );
    println!("\x1b[1;36m\u{255A}{}\u{255D}\x1b[0m", "\u{2550}".repeat(58));
    println!();

    // Step 1 — create ~/.anvil/ structure
    let home = anvil_home();
    info(&format!("Creating {} ...", home.display()));
    match create_directory_structure(&home) {
        Ok(()) => success(&format!("Directory {} ready.", home.display())),
        Err(e) => {
            eprintln!("  \x1b[31mError:\x1b[0m {e}");
            std::process::exit(1);
        }
    }

    // Step 2 — provider selection
    println!();
    println!("\x1b[1;33mStep 1 of 4: Choose your AI provider\x1b[0m");
    println!("\x1b[33m{}\x1b[0m", "\u{2501}".repeat(40));
    println!("  1) Anthropic (Claude) — recommended");
    println!("  2) OpenAI (GPT-4, o-series)");
    println!("  3) Google (Gemini)");
    println!("  4) xAI (Grok)");
    println!("  5) Ollama (local, no API key)");
    println!();

    let provider = loop {
        let answer = prompt("  Enter number or name [1]: ");
        let choice = if answer.is_empty() { "1".to_string() } else { answer };
        if let Some(p) = parse_provider(&choice) {
            break p;
        }
        warn("Invalid choice — enter 1–5 or the provider name.");
    };

    success(&format!("Provider: {}", provider.display()));

    // Step 3 — API key entry
    println!();
    println!("\x1b[1;33mStep 2 of 4: Configure API key\x1b[0m");
    println!("\x1b[33m{}\x1b[0m", "\u{2501}".repeat(40));

    let mut api_key = String::new();
    let mut ollama_url: Option<String> = None;

    match &provider {
        Provider::Ollama => {
            info("Ollama uses a local server — no API key needed.");
            let url = prompt("  Ollama URL [http://localhost:11434]: ");
            ollama_url = if url.is_empty() {
                Some("http://localhost:11434".to_string())
            } else {
                Some(url)
            };
        }
        _ => {
            if let Some(env_key) = provider.key_env() {
                info(&format!("Get your key at: {}", provider.docs_url()));
                info(&format!(
                    "It will be stored in {} (plaintext, chmod 0600). Migrate to the",
                    home.join("config.json").display()
                ));
                info("encrypted vault later with `/vault setup` then `/vault store <type> <label>`.");
                println!();

                // Check if already in environment
                if let Ok(env_val) = std::env::var(env_key) {
                    if !env_val.is_empty() {
                        let use_env = prompt(&format!("  {env_key} found in environment — use it? [Y/n] "));
                        if use_env.is_empty()
                            || matches!(use_env.to_ascii_lowercase().as_str(), "y" | "yes")
                        {
                            api_key = env_val;
                        }
                    }
                }

                if api_key.is_empty() {
                    // No-echo input so the key never appears on screen or in scrollback.
                    match rpassword::prompt_password("  Paste your API key (hidden): ") {
                        Ok(k) => api_key = k.trim().to_string(),
                        Err(e) => warn(&format!("Could not read key: {e}")),
                    }
                }

                if api_key.is_empty() {
                    warn("No API key entered — skipping. Run `anvil --setup` again to add one.");
                } else {
                    success("API key recorded.");
                }
            }
        }
    }

    // Write config
    if let Err(e) = write_provider_config(&home, &provider, &api_key, ollama_url.as_deref()) {
        warn(&format!("Could not write config: {e}"));
    } else {
        success("Config written.");
    }

    // Step 4 — Ollama optional install (if not already chosen)
    if provider != Provider::Ollama && !check_ollama_installed() {
        println!();
        println!("\x1b[1;33mStep 3 of 4: Ollama (optional)\x1b[0m");
        println!("\x1b[33m{}\x1b[0m", "\u{2501}".repeat(40));

        if prompt_ollama_install() {
            match install_ollama() {
                Ok(()) => success("Ollama installed."),
                Err(e) => warn(&format!("Ollama install failed: {e}")),
            }
        }
    }

    // Step 4 (or 5) — shell completions
    println!();
    println!("\x1b[1;33mStep 4 of 4: Shell completions\x1b[0m");
    println!("\x1b[33m{}\x1b[0m", "\u{2501}".repeat(40));

    let installed_completions = install_completions(&home);
    if installed_completions.is_empty() {
        warn("Could not install completions automatically.");
        info("To install manually, see: https://anvilhub.culpur.net/docs/completions");
    } else {
        for path in &installed_completions {
            success(&format!("Completions: {path}"));
        }
        info("Restart your shell for completions to take effect.");
    }

    // Summary
    println!();
    println!("\x1b[1;32m\u{2554}{}\u{2557}\x1b[0m", "\u{2550}".repeat(58));
    println!("\x1b[1;32m\u{2551}\x1b[0m  Setup complete! Run \x1b[1manvil\x1b[0m to start.{}  \x1b[1;32m\u{2551}\x1b[0m", " ".repeat(22));
    println!("\x1b[1;32m\u{255A}{}\u{255D}\x1b[0m", "\u{2550}".repeat(58));
    println!();
    info("Run `anvil --check` at any time to verify your installation.");
    println!();
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_provider_by_number() {
        assert_eq!(parse_provider("1"), Some(Provider::Anthropic));
        assert_eq!(parse_provider("2"), Some(Provider::OpenAi));
        assert_eq!(parse_provider("3"), Some(Provider::Google));
        assert_eq!(parse_provider("4"), Some(Provider::XAi));
        assert_eq!(parse_provider("5"), Some(Provider::Ollama));
    }

    #[test]
    fn parse_provider_by_name() {
        assert_eq!(parse_provider("anthropic"), Some(Provider::Anthropic));
        assert_eq!(parse_provider("claude"), Some(Provider::Anthropic));
        assert_eq!(parse_provider("openai"), Some(Provider::OpenAi));
        assert_eq!(parse_provider("google"), Some(Provider::Google));
        assert_eq!(parse_provider("gemini"), Some(Provider::Google));
        assert_eq!(parse_provider("xai"), Some(Provider::XAi));
        assert_eq!(parse_provider("grok"), Some(Provider::XAi));
        assert_eq!(parse_provider("ollama"), Some(Provider::Ollama));
    }

    #[test]
    fn parse_provider_unknown() {
        assert_eq!(parse_provider(""), None);
        assert_eq!(parse_provider("foobar"), None);
        assert_eq!(parse_provider("99"), None);
    }

    #[test]
    fn provider_name_is_lowercase() {
        for p in &[
            Provider::Anthropic,
            Provider::OpenAi,
            Provider::Google,
            Provider::XAi,
            Provider::Ollama,
        ] {
            let name = p.name();
            assert_eq!(name, name.to_ascii_lowercase(), "provider name must be lowercase");
        }
    }

    #[test]
    fn ollama_has_no_env_key() {
        assert!(Provider::Ollama.key_env().is_none());
    }

    #[test]
    fn non_ollama_providers_have_env_key() {
        for p in &[
            Provider::Anthropic,
            Provider::OpenAi,
            Provider::Google,
            Provider::XAi,
        ] {
            assert!(p.key_env().is_some(), "{} must have an env key", p.name());
        }
    }

    #[test]
    fn write_provider_config_roundtrip() {
        // Write to a temp dir and verify the JSON is sane.
        let dir = std::env::temp_dir().join(format!("anvil-setup-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let _ = create_directory_structure(&dir);

        write_provider_config(&dir, &Provider::Anthropic, "sk-test-key", None).unwrap();

        let data = fs::read_to_string(dir.join("config.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&data).unwrap();

        assert_eq!(
            val.pointer("/providers/anthropic/api_key").and_then(|v| v.as_str()),
            Some("sk-test-key")
        );
        assert!(val.get("default_model").is_some());

        // Cleanup
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_provider_config_ollama_writes_url() {
        let dir = std::env::temp_dir().join(format!("anvil-setup-ollama-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let _ = create_directory_structure(&dir);

        write_provider_config(
            &dir,
            &Provider::Ollama,
            "",
            Some("http://localhost:11434"),
        )
        .unwrap();

        let data = fs::read_to_string(dir.join("config.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&data).unwrap();

        assert_eq!(
            val.pointer("/providers/ollama/url").and_then(|v| v.as_str()),
            Some("http://localhost:11434")
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
