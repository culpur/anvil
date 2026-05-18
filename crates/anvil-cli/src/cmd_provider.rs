// Edition 2024: env::set_var/remove_var require unsafe
#![allow(unsafe_code)]

//! Provider, search, goal, agent, share, and hub command handlers for `impl LiveCli`.
//!
//! Extracted from `main.rs` to reduce file size. No behaviour is changed.
//!
//! Contains:
//!   - `run_release_doctor`     — `/doctor release` pre-flight checks
//!   - `format_tokens`          — `/tokens` token breakdown report
//!   - `print_status`           — `/status` session stats (REPL path, prints to stdout)
//!   - `run_provider_command`   — `/provider [list|<name>|login]`
//!   - `run_inline_login`       — `/login [provider]` OAuth/API-key flow
//!   - `run_search_command`     — `/search <query>`
//!   - `format_search_tool_result` — thin wrapper over cmd_static
//!   - `run_failover_command`   — thin wrapper over cmd_static
//!   - `run_goal_command`       — `/goal [new|list|resume|pause|done|show]`
//!   - `run_agent_command`      — `/agent [traits|compose]`
//!   - `run_share_command_repl` — `/share` in batch/print REPL mode
//!   - `run_hub_command`        — `/hub [search|install|info|skills|…]`

use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{self, Write};
use std::process::Command;
use std::time::Duration;

use api::{detect_provider_kind, provider_display_name, ProviderKind};
use commands::{
    bundled_catalogue, compose_agent, format_agent_compose_empty_task_usage,
    format_agent_compose_empty_traits_usage, format_agent_compose_error,
    format_agent_compose_summary, format_traits_listing, AgentSubcommand,
};
use runtime::{format_package_detail, format_package_list, BlockingHubClient, pricing_for_model};

use crate::{
    anvil_home_dir, build_runtime_with_tui_slot, format_status_report, status_context,
    StatusUsage, LiveCli,
};
use crate::auth::{
    query_anthropic_models, run_anthropic_login, run_ollama_setup, run_openai_apikey_setup,
};
use crate::cmd_static;

impl LiveCli {
    // ── /doctor release ───────────────────────────────────────────────────────

    /// Pre-flight checks for the release pipeline: version, release notes,
    /// clean working tree, tag agreement, brew shadow, gh auth.
    pub(crate) fn run_release_doctor() -> String {
        let mut lines = vec!["Anvil Release Doctor".to_string(), String::new()];

        // 1. Version from Cargo.toml
        let workspace_root = env::current_dir().unwrap_or_default();
        let cargo_path = workspace_root.join("Cargo.toml");
        let version: Option<String> = fs::read_to_string(&cargo_path)
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("version = "))
                    .and_then(|l| l.split('"').nth(1).map(str::to_string))
            });
        match &version {
            Some(v) if v.split('.').count() == 3 => {
                lines.push(format!("  ✓ Cargo.toml version: {v}"));
            }
            Some(v) => lines.push(format!("  ✗ Cargo.toml version not semver-shaped: {v}")),
            None => lines.push("  ✗ Cargo.toml version not found".to_string()),
        }

        let tag = version.as_ref().map(|v| format!("v{v}"));

        // 2. RELEASE-NOTES file
        if let Some(t) = &tag {
            let notes_path = workspace_root.join(format!("RELEASE-NOTES-{t}.md"));
            match fs::read_to_string(&notes_path) {
                Ok(content) => {
                    let line_count = content.lines().count();
                    if line_count > 10 {
                        lines.push(format!(
                            "  ✓ RELEASE-NOTES-{t}.md present ({line_count} lines)"
                        ));
                    } else {
                        lines.push(format!(
                            "  ✗ RELEASE-NOTES-{t}.md only {line_count} lines (write a real changelog)"
                        ));
                    }
                }
                Err(_) => lines.push(format!(
                    "  ✗ RELEASE-NOTES-{t}.md missing — release.sh will hard-fail"
                )),
            }
        }

        // 3. Clean working tree
        let dirty = Command::new("git")
            .args(["status", "--porcelain"])
            .output()
            .ok()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(true);
        lines.push(format!(
            "  {} Working tree clean",
            if dirty { "✗" } else { "✓" }
        ));

        // 4 + 5. Tag agreement (local HEAD vs tag, local tag vs remote)
        if let Some(t) = &tag {
            let local_tag_sha = Command::new("git")
                .args(["rev-parse", "--verify", t])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
            let head_sha = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

            match (&local_tag_sha, &head_sha) {
                (Some(t_sha), Some(h_sha)) if t_sha == h_sha => {
                    lines.push(format!("  ✓ Tag {t} points at HEAD ({})", &t_sha[..7.min(t_sha.len())]));
                }
                (Some(t_sha), Some(h_sha)) => {
                    lines.push(format!(
                        "  ✗ Tag {t} ({}) != HEAD ({}) — re-tag before releasing",
                        &t_sha[..7.min(t_sha.len())],
                        &h_sha[..7.min(h_sha.len())]
                    ));
                }
                (None, _) => {
                    lines.push(format!("  - Tag {t} not yet created locally (will be created during release)"));
                }
                _ => {}
            }

            // Remote tag
            let remote_tag = Command::new("git")
                .args(["ls-remote", "--tags", "origin", &format!("refs/tags/{t}")])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty());
            match (&local_tag_sha, &remote_tag) {
                (Some(local_sha), Some(remote_line)) => {
                    let remote_sha = remote_line.split_whitespace().next().unwrap_or("");
                    if remote_sha == local_sha {
                        lines.push(format!("  ✓ Remote tag {t} matches local"));
                    } else {
                        lines.push(format!(
                            "  ✗ Remote tag {t} ({}) != local ({}) — `git push --force-with-lease origin {t}` after fixing",
                            &remote_sha[..7.min(remote_sha.len())],
                            &local_sha[..7.min(local_sha.len())]
                        ));
                    }
                }
                (Some(_), None) => {
                    lines.push(format!("  - Remote tag {t} not yet pushed (release.sh will push)"));
                }
                _ => {}
            }
        }

        // 6. Brew shadow check (the /opt/homebrew/bin/anvil incident)
        let brew_path = std::path::Path::new("/opt/homebrew/bin/anvil");
        if brew_path.exists() {
            match fs::symlink_metadata(brew_path).map(|m| m.file_type().is_symlink()) {
                Ok(true) => lines.push("  ✓ /opt/homebrew/bin/anvil is a brew symlink".to_string()),
                Ok(false) => lines.push(
                    "  ✗ /opt/homebrew/bin/anvil is a regular file shadowing brew — `rm` it then `brew link --overwrite anvil`".to_string(),
                ),
                Err(_) => {}
            }
        } else {
            lines.push("  - /opt/homebrew/bin/anvil not present (brew install pending)".to_string());
        }

        // 7. gh auth
        let gh_ok = Command::new("gh")
            .args(["auth", "status"])
            .output()
            .ok()
            .map(|o| o.status.success())
            .unwrap_or(false);
        lines.push(format!(
            "  {} gh auth status",
            if gh_ok { "✓" } else { "✗" }
        ));

        lines.join("\n")
    }

    // ── /tokens ───────────────────────────────────────────────────────────────

    /// Token breakdown report for `/tokens`.
    pub(crate) fn format_tokens(&self) -> String {
        let cumulative = self.active_runtime().usage().cumulative_usage();
        let latest = self.active_runtime().usage().current_turn_usage();
        let turns = self.active_runtime().usage().turns();
        let est = self.active_runtime().estimated_tokens();

        // Context window for the current model.
        let ctx_window: usize = 200_000;
        #[allow(clippy::cast_precision_loss)]
        let ctx_pct = if ctx_window > 0 {
            (est as f64 / ctx_window as f64 * 100.0).min(100.0)
        } else {
            0.0
        };

        let pricing = pricing_for_model(&self.model);
        let cost_lines = cumulative.summary_lines_for_model("Cumulative", Some(&self.model));
        let latest_lines = latest.summary_lines_for_model("Last turn  ", Some(&self.model));

        let mut lines = vec![
            "Token breakdown".to_string(),
            String::new(),
            format!("  Turns completed  {turns}"),
            format!("  Context window   ~{est} / {ctx_window} tokens  ({ctx_pct:.1}%)"),
            String::new(),
        ];
        for line in &latest_lines {
            lines.push(format!("  {line}"));
        }
        lines.push(String::new());
        for line in &cost_lines {
            lines.push(format!("  {line}"));
        }
        if let Some(p) = pricing {
            lines.push(String::new());
            lines.push(format!(
                "  Pricing ({})  input=${}/Mtok  output=${}/Mtok",
                self.model,
                p.input_cost_per_million,
                p.output_cost_per_million,
            ));
        }
        lines.join("\n")
    }

    // ── /status (REPL print path) ─────────────────────────────────────────────

    /// Print the session status report to stdout. Used by the batch/print REPL
    /// path (`handle_repl_command`). The TUI path returns a string via
    /// `run_command_for_tui`.
    pub(crate) fn print_status(&self) {
        let cumulative = self.active_runtime().usage().cumulative_usage();
        let latest = self.active_runtime().usage().current_turn_usage();
        println!(
            "{}",
            format_status_report(
                &self.model,
                StatusUsage {
                    message_count: self.active_runtime().session().messages.len(),
                    turns: self.active_runtime().usage().turns(),
                    latest,
                    cumulative,
                    estimated_tokens: self.active_runtime().estimated_tokens(),
                },
                self.permission_mode.as_str(),
                &match status_context(Some(&self.session.path)) {
                    Ok(ctx) => ctx,
                    Err(e) => {
                        eprintln!("status context error: {e}");
                        return;
                    }
                },
            )
        );
    }

    // ── /provider ─────────────────────────────────────────────────────────────

    /// Handle `/provider` command — show, switch, or list provider models.
    pub(crate) fn run_provider_command(&mut self, action: Option<&str>) -> String {
        let current_kind = detect_provider_kind(&self.model);
        let current_name = provider_display_name(current_kind);

        match action {
            None | Some("") => {
                // Show current provider and available providers
                let mut out = format!("Current provider: {current_name}\n");
                let _ = write!(out, "Current model: {}\n\n", self.model);
                out.push_str("Available providers:\n");
                out.push_str("  anthropic  — Claude models (claude-opus-4-6, claude-sonnet-4-6, claude-haiku-4-5)\n");
                out.push_str("  openai     — GPT/o-series (gpt-5.4-mini, gpt-5, o3, o4-mini, …)\n");
                out.push_str("  ollama     — Local models (llama3.2, mistral, qwen, gemma, etc.)\n\n");
                out.push_str("Usage:\n");
                out.push_str("  /provider list        — List models for current provider\n");
                out.push_str("  /provider anthropic   — Switch to Anthropic\n");
                out.push_str("  /provider openai      — Switch to OpenAI\n");
                out.push_str("  /provider ollama      — Switch to Ollama (local)\n");
                out.push_str("  /provider login       — Login/refresh current provider\n");
                out.push_str("  /login                — Same as /provider login\n");
                out.push_str("  /login anthropic      — Login to a specific provider\n");
                out.push_str("  /model <name>         — Switch to a specific model\n");
                out
            }
            Some("list" | "ls" | "models") => {
                // List models for current provider
                let mut out = format!("Models for {current_name}:\n\n");
                match current_kind {
                    ProviderKind::AnvilApi => {
                        // Try live API query first
                        let live_models = query_anthropic_models();
                        if live_models.is_empty() {
                            out.push_str("  claude-opus-4-6          Opus 4.6 (1M context, most capable)\n");
                            out.push_str("  claude-sonnet-4-6        Sonnet 4.6 (1M context, balanced)\n");
                            out.push_str("  claude-haiku-4-5         Haiku 4.5 (200K context, fast)\n");
                            out.push_str("\n  (Live model list unavailable — run /login anthropic to refresh)\n");
                        } else {
                            for (id, name) in &live_models {
                                let _ = writeln!(out, "  {id:<30} {name}");
                            }
                        }
                    }
                    ProviderKind::OpenAi => {
                        out.push_str("  Frontier:\n");
                        out.push_str("    gpt-5.4                GPT-5.4 (flagship)\n");
                        out.push_str("    gpt-5.4-pro            GPT-5.4 Pro (smarter, more precise)\n");
                        out.push_str("    gpt-5.4-mini           GPT-5.4 Mini (coding, computer use, subagents)\n");
                        out.push_str("    gpt-5.4-nano           GPT-5.4 Nano (cheapest frontier)\n");
                        out.push_str("    gpt-5                  GPT-5 (reasoning)\n");
                        out.push_str("    gpt-5-mini             GPT-5 Mini (cost-sensitive)\n");
                        out.push_str("    gpt-5-nano             GPT-5 Nano (fastest)\n");
                        out.push_str("  Coding:\n");
                        out.push_str("    gpt-5-codex            GPT-5 Codex (agentic coding)\n");
                        out.push_str("    gpt-5.3-codex          GPT-5.3 Codex (most capable coding)\n");
                        out.push_str("  Reasoning:\n");
                        out.push_str("    o3                     o3 (complex reasoning)\n");
                        out.push_str("    o3-pro                 o3 Pro (more compute)\n");
                        out.push_str("    o3-mini                o3 Mini (fast reasoning)\n");
                        out.push_str("    o4-mini                o4 Mini (cost-efficient reasoning)\n");
                        out.push_str("  Research:\n");
                        out.push_str("    o3-deep-research       o3 Deep Research\n");
                        out.push_str("    o4-mini-deep-research  o4 Mini Deep Research\n");
                        out.push_str("  Image:\n");
                        out.push_str("    gpt-image-1.5          GPT Image 1.5 (best)\n");
                        out.push_str("    gpt-image-1            GPT Image 1\n");
                        out.push_str("    gpt-image-1-mini       GPT Image 1 Mini (cost-efficient)\n");
                        out.push_str("  Previous gen:\n");
                        out.push_str("    gpt-4.1                GPT-4.1\n");
                        out.push_str("    gpt-4.1-mini           GPT-4.1 Mini\n");
                        out.push_str("    gpt-4o                 GPT-4o\n");
                        out.push_str("    gpt-4o-mini            GPT-4o Mini\n");
                    }
                    ProviderKind::Ollama => {
                        // Query Ollama for available models
                        let ollama_url = std::env::var("OLLAMA_HOST")
                            .unwrap_or_else(|_| "http://localhost:11434".to_string());
                        match std::process::Command::new("curl")
                            .args(["-s", &format!("{ollama_url}/api/tags")])
                            .output()
                        {
                            Ok(output) if output.status.success() => {
                                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
                                    && let Some(models) = val.get("models").and_then(|m| m.as_array()) {
                                        for m in models {
                                            let name = m.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                                            let size = m.get("size").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                                            let gb = size / 1_000_000_000.0;
                                            let _ = writeln!(out, "  {name:<30} {gb:.1}GB");
                                        }
                                    }
                            }
                            _ => {
                                out.push_str("  (Ollama not running — start with `ollama serve`)\n");
                            }
                        }
                    }
                    ProviderKind::Xai => {
                        out.push_str("  grok-3                   Grok 3\n");
                        out.push_str("  grok-3-mini              Grok 3 Mini\n");
                    }
                    ProviderKind::Gemini => {
                        out.push_str("  gemini-2.5-pro           Gemini 2.5 Pro (1M context, thinking)\n");
                        out.push_str("  gemini-2.5-flash         Gemini 2.5 Flash (fast, 1M context)\n");
                        out.push_str("  gemini-2.0-flash         Gemini 2.0 Flash\n");
                        out.push_str("  gemini-1.5-pro           Gemini 1.5 Pro (2M context)\n");
                        out.push_str("  gemini-1.5-flash         Gemini 1.5 Flash\n");
                    }
                    _ => {
                        let display = api::provider_display_name(current_kind);
                        let _ = writeln!(
                            out,
                            "  (Use /model <tab> to browse live models for {display})"
                        );
                    }
                }
                out
            }
            Some("login") => {
                // `/provider login` — interactive login for current provider
                self.run_inline_login(None)
            }
            Some(action) if action.ends_with(" login") || action.starts_with("login ") => {
                // `/provider anthropic login` or `/provider login anthropic`
                let provider_name = action.replace("login", "").trim().to_string();
                if provider_name.is_empty() {
                    return self.run_inline_login(None);
                }
                self.run_inline_login(Some(&provider_name))
            }
            Some(provider) if provider.contains(' ') && provider.split_whitespace().any(|w| w == "login") => {
                let parts: Vec<&str> = provider.split_whitespace().filter(|w| *w != "login").collect();
                let name = parts.first().map(std::string::ToString::to_string);
                self.run_inline_login(name.as_deref())
            }
            Some(provider) => {
                // Switch provider — pick the default model for that provider
                let (new_model, name) = match provider.to_lowercase().as_str() {
                    "anthropic" | "claude" | "ant" => ("claude-sonnet-4-6", "Anthropic"),
                    "openai" | "gpt" | "oai" => ("gpt-5.4-mini", "OpenAI"),
                    "gemini" | "google" => ("gemini-2.5-flash", "Gemini"),
                    "ollama" | "local" => ("llama3.2", "Ollama"),
                    "xai" | "grok" => ("grok", "xAI"),
                    other => {
                        return format!("Unknown provider: {other}\nAvailable: anthropic, openai, gemini, ollama, xai");
                    }
                };

                match self.set_model(Some(new_model.to_string())) {
                    Ok(_) => {
                        format!("Switched to {name} ({new_model})")
                    }
                    Err(e) => {
                        format!("Failed to switch provider: {e}")
                    }
                }
            }
        }
    }

    // ── /login ────────────────────────────────────────────────────────────────

    /// `/login [provider]` or `/provider login` — refresh OAuth token from within REPL.
    /// Temporarily leaves the TUI to run the OAuth browser flow, then returns.
    pub(crate) fn run_inline_login(&self, provider: Option<&str>) -> String {
        let provider_name = provider.unwrap_or_else(|| {
            match detect_provider_kind(&self.model) {
                ProviderKind::AnvilApi => "anthropic",
                ProviderKind::OpenAi => "openai",
                ProviderKind::Gemini => "gemini",
                ProviderKind::Ollama => "ollama",
                ProviderKind::Xai => "xai",
                ProviderKind::Fireworks => "fireworks",
                ProviderKind::MiniMax => "minimax",
                ProviderKind::Groq => "groq",
                ProviderKind::Mistral => "mistral",
                ProviderKind::Perplexity => "perplexity",
                ProviderKind::DeepSeek => "deepseek",
                ProviderKind::TogetherAi => "togetherai",
                ProviderKind::DeepInfra => "deepinfra",
                ProviderKind::Chutes => "chutes",
                ProviderKind::Cerebras => "cerebras",
                ProviderKind::NvidiaNim => "nvidia-nim",
                ProviderKind::HuggingFace => "huggingface",
                ProviderKind::MoonshotAi => "moonshotai",
                ProviderKind::Nebius => "nebius",
                ProviderKind::Scaleway => "scaleway",
                ProviderKind::StackIt => "stackit",
                ProviderKind::Baseten => "baseten",
                ProviderKind::Cortecs => "cortecs",
                ProviderKind::Ai302 => "302ai",
                ProviderKind::Zai => "zai",
                ProviderKind::OpenRouter => "openrouter",
                ProviderKind::LmStudio => "lmstudio",
                ProviderKind::OpenCode => "opencode",
                ProviderKind::OpenCodeGo => "opencode-go",
                ProviderKind::Copilot => "copilot",
                ProviderKind::Azure => "azure",
                ProviderKind::Bedrock => "bedrock",
                ProviderKind::Alibaba => "alibaba",
                ProviderKind::Antigravity => "antigravity",
                ProviderKind::Cursor => "cursor",
            }
        });

        match provider_name.to_lowercase().as_str() {
            "anthropic" | "claude" => {
                let _ = crossterm::terminal::disable_raw_mode();
                crate::tui::leave_alt_screen_for_inline_op();

                println!("\n⚒ Anthropic Login\n");
                println!("  1) OAuth (browser login via claude.ai)");
                println!("  2) API Key\n");
                print!("Choice [1-2]: ");
                let _ = io::stdout().flush();
                let mut choice = String::new();
                let _ = io::stdin().read_line(&mut choice);

                let result = match choice.trim() {
                    "2" | "key" | "apikey" => {
                        run_openai_apikey_setup("Anthropic", "ANTHROPIC_API_KEY", "anthropic_api_key", "sk-ant-")
                    }
                    _ => run_anthropic_login(),
                };
                match result {
                    Ok(()) => {
                        println!("\n✓ Credentials saved. Press any key to return to Anvil.");
                    }
                    Err(e) => {
                        println!("\n✗ Login failed: {e}\nPress any key to return to Anvil.");
                    }
                }
                let _ = io::stdout().flush();
                let _ = crossterm::terminal::enable_raw_mode();
                if crossterm::event::poll(Duration::from_secs(60)).unwrap_or(false) {
                    let _ = crossterm::event::read();
                }
                crate::tui::restore_alt_screen();
                "Anthropic login complete. Token refreshed.".to_string()
            }
            "openai" | "gpt" => {
                let _ = crossterm::terminal::disable_raw_mode();
                crate::tui::leave_alt_screen_for_inline_op();

                println!("\n⚒ OpenAI API Key Setup\n");
                match run_openai_apikey_setup("OpenAI", "OPENAI_API_KEY", "openai_api_key", "sk-") {
                    Ok(()) => {
                        println!("\nPress any key to return to Anvil.");
                    }
                    Err(e) => {
                        println!("\n✗ Setup failed: {e}\nPress any key to return to Anvil.");
                    }
                }
                let _ = io::stdout().flush();
                let _ = crossterm::terminal::enable_raw_mode();
                if crossterm::event::poll(Duration::from_secs(60)).unwrap_or(false) {
                    let _ = crossterm::event::read();
                }
                crate::tui::restore_alt_screen();
                "OpenAI API key configured.".to_string()
            }
            "ollama" | "local" => {
                let _ = crossterm::terminal::disable_raw_mode();
                crate::tui::leave_alt_screen_for_inline_op();

                match run_ollama_setup() {
                    Ok(()) => {
                        println!("\nPress any key to return to Anvil.");
                    }
                    Err(e) => {
                        println!("\n✗ Setup failed: {e}\nPress any key to return to Anvil.");
                    }
                }
                let _ = io::stdout().flush();
                let _ = crossterm::terminal::enable_raw_mode();
                if crossterm::event::poll(Duration::from_secs(60)).unwrap_or(false) {
                    let _ = crossterm::event::read();
                }
                crate::tui::restore_alt_screen();
                "Ollama configured.".to_string()
            }
            // ── Group B: OpenAI-compatible API-key providers ─────────────────
            slug @ ("fireworks" | "minimax" | "groq" | "mistral" | "perplexity"
                | "deepseek" | "togetherai" | "deepinfra" | "chutes" | "cerebras"
                | "nvidia-nim" | "huggingface" | "moonshotai" | "nebius"
                | "scaleway" | "stackit" | "baseten" | "cortecs" | "302ai"
                | "zai" | "kimi" | "glm" | "openrouter" | "lmstudio"
                | "opencode" | "opencode-go" | "alibaba" | "dashscope"
                | "antigravity" | "cursor") => {
                let _ = crossterm::terminal::disable_raw_mode();
                crate::tui::leave_alt_screen_for_inline_op();

                let (display, env_var, creds_key, prefix) = match slug {
                    "fireworks"   => ("Fireworks",          "FIREWORKS_API_KEY",    "fireworks_api_key",    "fw-"),
                    "minimax"     => ("MiniMax",             "MINIMAX_API_KEY",      "minimax_api_key",      ""),
                    "groq"        => ("Groq",                "GROQ_API_KEY",         "groq_api_key",         "gsk_"),
                    "mistral"     => ("Mistral",             "MISTRAL_API_KEY",      "mistral_api_key",      ""),
                    "perplexity"  => ("Perplexity",          "PPLX_API_KEY",         "perplexity_api_key",   "pplx-"),
                    "deepseek"    => ("DeepSeek",            "DEEPSEEK_API_KEY",     "deepseek_api_key",     "sk-"),
                    "togetherai"  => ("Together AI",         "TOGETHER_API_KEY",     "together_api_key",     ""),
                    "deepinfra"   => ("DeepInfra",           "DEEPINFRA_API_KEY",    "deepinfra_api_key",    ""),
                    "chutes"      => ("Chutes",              "CHUTES_API_KEY",       "chutes_api_key",       ""),
                    "cerebras"    => ("Cerebras",            "CEREBRAS_API_KEY",     "cerebras_api_key",     "csk-"),
                    "nvidia-nim"  => ("NVIDIA NIM",          "NVIDIA_API_KEY",       "nvidia_api_key",       "nvapi-"),
                    "huggingface" => ("Hugging Face",        "HF_TOKEN",             "huggingface_token",    "hf_"),
                    "moonshotai"  => ("Moonshot AI",         "MOONSHOT_API_KEY",     "moonshot_api_key",     "sk-"),
                    "nebius"      => ("Nebius",              "NEBIUS_API_KEY",       "nebius_api_key",       ""),
                    "scaleway"    => ("Scaleway",            "SCALEWAY_API_KEY",     "scaleway_api_key",     ""),
                    "stackit"     => ("STACKIT",             "STACKIT_API_KEY",      "stackit_api_key",      ""),
                    "baseten"     => ("Baseten",             "BASETEN_API_KEY",      "baseten_api_key",      ""),
                    "cortecs"     => ("Cortecs",             "CORTECS_API_KEY",      "cortecs_api_key",      ""),
                    "302ai"       => ("302.ai",              "AI302_API_KEY",        "ai302_api_key",        ""),
                    "zai" | "kimi" | "glm" => ("Zai/Kimi/GLM", "ZAI_API_KEY",      "zai_api_key",          ""),
                    "openrouter"  => ("OpenRouter",          "OPENROUTER_API_KEY",   "openrouter_api_key",   "sk-or-"),
                    "lmstudio"    => ("LM Studio",           "LMSTUDIO_API_KEY",     "lmstudio_api_key",     ""),
                    "opencode"    => ("OpenCode",            "OPENCODE_API_KEY",     "opencode_api_key",     ""),
                    "opencode-go" => ("OpenCode Go",         "OPENCODE_GO_API_KEY",  "opencode_go_api_key",  ""),
                    "alibaba" | "dashscope" => ("Alibaba DashScope", "DASHSCOPE_API_KEY", "dashscope_api_key", "sk-"),
                    "antigravity" => ("Antigravity",         "ANTIGRAVITY_API_KEY",  "antigravity_api_key",  ""),
                    "cursor"      => ("Cursor",              "CURSOR_API_KEY",       "cursor_api_key",       ""),
                    _             => ("Unknown",             "UNKNOWN_API_KEY",      "unknown_api_key",      ""),
                };

                println!("\n⚒ {display} API Key Setup\n");
                match run_openai_apikey_setup(display, env_var, creds_key, prefix) {
                    Ok(()) => println!("\nPress any key to return to Anvil."),
                    Err(e) => println!("\n✗ Setup failed: {e}\nPress any key to return to Anvil."),
                }
                let _ = io::stdout().flush();
                let _ = crossterm::terminal::enable_raw_mode();
                if crossterm::event::poll(Duration::from_secs(60)).unwrap_or(false) {
                    let _ = crossterm::event::read();
                }
                crate::tui::restore_alt_screen();
                format!("{display} API key configured.")
            }
            // ── Copilot: GitHub device flow ───────────────────────────────────
            "copilot" | "github-copilot" => {
                let _ = crossterm::terminal::disable_raw_mode();
                crate::tui::leave_alt_screen_for_inline_op();

                println!("\n⚒ GitHub Copilot Login\n");
                println!("  Running GitHub device flow authentication...\n");

                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                match rt.block_on(api::copilot_run_device_flow()) {
                    Ok(token_set) => {
                        match api::save_copilot_token(&token_set) {
                            Ok(()) => println!("\n✓ GitHub Copilot token saved. Press any key to return."),
                            Err(e) => println!("\n✗ Failed to save token: {e}\nPress any key to return."),
                        }
                    }
                    Err(e) => println!("\n✗ Device flow failed: {e}\nPress any key to return."),
                }
                let _ = io::stdout().flush();
                let _ = crossterm::terminal::enable_raw_mode();
                if crossterm::event::poll(Duration::from_secs(60)).unwrap_or(false) {
                    let _ = crossterm::event::read();
                }
                crate::tui::restore_alt_screen();
                "GitHub Copilot login complete.".to_string()
            }
            // ── Azure: endpoint + key ─────────────────────────────────────────
            "azure" => {
                let _ = crossterm::terminal::disable_raw_mode();
                crate::tui::leave_alt_screen_for_inline_op();

                println!("\n⚒ Azure OpenAI Setup\n");
                println!("  Set these environment variables (or add to ~/.anvil/credentials.json):\n");
                println!("    AZURE_OPENAI_ENDPOINT        — e.g. https://MY.openai.azure.com");
                println!("    AZURE_OPENAI_DEPLOYMENT_NAME — e.g. gpt-4o");
                println!("    AZURE_OPENAI_API_VERSION     — e.g. 2025-01-01-preview");
                println!("    AZURE_OPENAI_API_KEY         — your api-key  (or set AZURE_AD_TOKEN)");
                println!("    AZURE_AD_TOKEN               — AAD bearer token (optional, overrides api-key)\n");
                println!("  Press any key to return to Anvil.");
                let _ = io::stdout().flush();
                let _ = crossterm::terminal::enable_raw_mode();
                if crossterm::event::poll(Duration::from_secs(60)).unwrap_or(false) {
                    let _ = crossterm::event::read();
                }
                crate::tui::restore_alt_screen();
                "Azure setup instructions displayed.".to_string()
            }
            // ── Bedrock: AWS credentials ─────────────────────────────────────
            "bedrock" | "aws" | "aws-bedrock" => {
                let _ = crossterm::terminal::disable_raw_mode();
                crate::tui::leave_alt_screen_for_inline_op();

                println!("\n⚒ AWS Bedrock Setup\n");
                println!("  Set these environment variables:\n");
                println!("    AWS_ACCESS_KEY_ID     — your access key");
                println!("    AWS_SECRET_ACCESS_KEY — your secret key");
                println!("    AWS_REGION            — e.g. us-east-1");
                println!("    AWS_SESSION_TOKEN     — (optional, for temporary credentials)\n");
                println!("  Alternatively configure via `aws configure` (standard AWS credential chain).\n");
                println!("  Press any key to return to Anvil.");
                let _ = io::stdout().flush();
                let _ = crossterm::terminal::enable_raw_mode();
                if crossterm::event::poll(Duration::from_secs(60)).unwrap_or(false) {
                    let _ = crossterm::event::read();
                }
                crate::tui::restore_alt_screen();
                "AWS Bedrock setup instructions displayed.".to_string()
            }
            "gemini" | "google" => {
                let _ = crossterm::terminal::disable_raw_mode();
                crate::tui::leave_alt_screen_for_inline_op();

                println!("\n⚒ Gemini API Key Setup\n");
                match run_openai_apikey_setup("Gemini", "GEMINI_API_KEY", "gemini_api_key", "AIza") {
                    Ok(()) => println!("\nPress any key to return to Anvil."),
                    Err(e) => println!("\n✗ Setup failed: {e}\nPress any key to return to Anvil."),
                }
                let _ = io::stdout().flush();
                let _ = crossterm::terminal::enable_raw_mode();
                if crossterm::event::poll(Duration::from_secs(60)).unwrap_or(false) {
                    let _ = crossterm::event::read();
                }
                crate::tui::restore_alt_screen();
                "Gemini API key configured.".to_string()
            }
            "xai" | "grok" => {
                let _ = crossterm::terminal::disable_raw_mode();
                crate::tui::leave_alt_screen_for_inline_op();

                println!("\n⚒ xAI API Key Setup\n");
                match run_openai_apikey_setup("xAI", "XAI_API_KEY", "xai_api_key", "xai-") {
                    Ok(()) => println!("\nPress any key to return to Anvil."),
                    Err(e) => println!("\n✗ Setup failed: {e}\nPress any key to return to Anvil."),
                }
                let _ = io::stdout().flush();
                let _ = crossterm::terminal::enable_raw_mode();
                if crossterm::event::poll(Duration::from_secs(60)).unwrap_or(false) {
                    let _ = crossterm::event::read();
                }
                crate::tui::restore_alt_screen();
                "xAI API key configured.".to_string()
            }
            other => {
                format!("Unknown provider: {other}. Use /provider list to see all providers.")
            }
        }
    }

    // ── /search ───────────────────────────────────────────────────────────────

    /// `/search` — multi-provider web search.
    pub(crate) fn run_search_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();

        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /search <query>                      Search with the default provider",
                "  /search provider <name> <query>      Search with a specific provider",
                "  /search providers                    List all configured providers",
                "  /search config <provider> <k> <v>   Set a provider config value",
                "",
                "Provider names: duckduckgo, tavily, exa, searxng, brave, google, perplexity, bing",
            ]
            .join("\n");
        }

        // `/search providers`
        if args == "providers" {
            let engine = runtime::SearchEngine::from_env_and_config();
            return runtime::format_provider_list(&engine.list_providers());
        }

        // `/search provider <name> <query>`
        if let Some(rest) = args.strip_prefix("provider ") {
            let mut parts = rest.splitn(2, ' ');
            let provider_name = parts.next().unwrap_or("").trim();
            let query = parts.next().unwrap_or("").trim();
            if query.is_empty() {
                return format!("Usage: /search provider {provider_name} <query>");
            }
            let input = serde_json::json!({
                "query": query,
                "provider": provider_name,
            });
            return self.format_search_tool_result(query, &input);
        }

        // `/search config <provider> <key> <value>` — runtime config write
        if let Some(rest) = args.strip_prefix("config ") {
            let parts: Vec<&str> = rest.splitn(3, ' ').collect();
            if parts.len() < 3 {
                return "Usage: /search config <provider> <key> <value>".to_string();
            }
            // For now, surface a note — persistent config writes go to ~/.anvil/search.json.
            return format!(
                "To configure provider '{}', set {} = {} in ~/.anvil/search.json",
                parts[0], parts[1], parts[2]
            );
        }

        // `/search <query>` — default provider
        let input = serde_json::json!({ "query": args });
        self.format_search_tool_result(args, &input)
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn format_search_tool_result(&self, query: &str, input: &serde_json::Value) -> String {
        cmd_static::format_search_tool_result(query, input)
    }

    /// `/failover` — AI provider failover chain management.
    #[allow(clippy::unused_self)]
    pub(crate) fn run_failover_command(&self, action: Option<&str>) -> String {
        cmd_static::run_failover_command(action)
    }

    // ── /goal ─────────────────────────────────────────────────────────────────

    /// Handle `/goal [new "<desc>"|list|resume <id>|pause [<id>]|done [<id>]|show [<id>]]`.
    pub(crate) fn run_goal_command(&mut self, args: Option<&str>) -> String {
        use runtime::{GoalManager, GoalError, format_goal_list, format_goal_show,
                      GOAL_DESCRIPTION_MAX};

        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let mut mgr = GoalManager::new(cwd);

        let raw = args.unwrap_or("").trim();

        // No args → alias for list
        if raw.is_empty() || raw == "list" || raw == "ls" {
            let goals = mgr.list().unwrap_or_default();
            return format_goal_list(&goals);
        }

        let mut iter = raw.splitn(2, char::is_whitespace);
        let sub = iter.next().unwrap_or("list");
        let rest = iter.next().unwrap_or("").trim();

        match sub {
            "new" => {
                let desc = rest
                    .trim_start_matches('"')
                    .trim_end_matches('"')
                    .trim();
                if desc.is_empty() {
                    return "Usage: /goal new \"<description>\"".to_string();
                }
                // CC-139-F2 unify: auto-link the active session so the goal
                // appears in both Anvil's project view AND CC-style
                // session lookups.
                match mgr.new_goal_for_session(desc, self.session_id()) {
                    Ok(goal) => format!(
                        "Goal created: {}\nStatus: {}\nSession-linked: {}\n{}",
                        goal.id, goal.status, self.session_id(), goal.description
                    ),
                    Err(GoalError::DescriptionTooLong { len, .. }) => format!(
                        "Description too long ({len} chars). Maximum is {GOAL_DESCRIPTION_MAX} chars."
                    ),
                    Err(e) => format!("Error: {e}"),
                }
            }
            "resume" => {
                if rest.is_empty() {
                    return "Usage: /goal resume <id>".to_string();
                }
                match mgr.resume(rest) {
                    Ok(goal) => format!(
                        "Resumed: {} ({})\n{}",
                        goal.id, goal.status, goal.description
                    ),
                    Err(GoalError::GoalNotFound(id)) => format!("Goal not found: {id}"),
                    Err(e) => format!("Error: {e}"),
                }
            }
            "pause" => {
                let id_opt = if rest.is_empty() { None } else { Some(rest) };
                match mgr.pause(id_opt) {
                    Ok(goal) => format!("Paused: {} ({})", goal.id, goal.status),
                    Err(GoalError::NoActiveGoal) => "No active goal to pause.".to_string(),
                    Err(GoalError::GoalNotFound(id)) => format!("Goal not found: {id}"),
                    Err(e) => format!("Error: {e}"),
                }
            }
            "done" => {
                let id_opt = if rest.is_empty() { None } else { Some(rest) };
                match mgr.done(id_opt) {
                    Ok(goal) => format!("Done: {} ({})", goal.id, goal.status),
                    Err(GoalError::NoActiveGoal) => "No active goal to mark done.".to_string(),
                    Err(GoalError::GoalNotFound(id)) => format!("Goal not found: {id}"),
                    Err(e) => format!("Error: {e}"),
                }
            }
            "show" => {
                let id_opt = if rest.is_empty() { None } else { Some(rest) };
                match mgr.show(id_opt) {
                    Ok(goal) => format_goal_show(&goal),
                    Err(GoalError::NoActiveGoal) => {
                        "No active goal. Use /goal list to see all goals.".to_string()
                    }
                    Err(GoalError::GoalNotFound(id) | GoalError::NotFound(id)) => {
                        format!("Goal not found: {id}")
                    }
                    Err(e) => format!("Error: {e}"),
                }
            }
            other => format!(
                "Unknown goal subcommand: '{other}'. \
                 Usage: /goal [new|list|resume|pause|done|show]"
            ),
        }
    }

    // ── /agent ────────────────────────────────────────────────────────────────

    /// Handle `/agent compose <traits> "<task>"` and `/agent traits`.
    ///
    /// Returns a user-facing message as `Ok(String)`.  The caller decides
    /// where the message goes:
    /// * TUI mode → `tui.push_system(msg)` so the line lands in the
    ///   ratatui scrollback and the back-buffer stays consistent.
    /// * Headless `--print` / batch mode → `println!("{msg}")`.
    ///
    /// Task #624 (v2.2.14 Phase 1): the previous version of this function
    /// wrote directly to stdout with `println!`, which corrupted the visible
    /// TUI because bytes went behind the alt-screen and ratatui's diff
    /// renderer never saw them.  Subsequent input/output stopped rendering
    /// until the TUI exited.  See `feedback-tui-stdout-anti-pattern.md`.
    ///
    /// For `compose`: loads the bundled catalogue, calls `compose_agent`,
    /// runs the task as a subagent turn (via `run_turn`, which already
    /// routes through the TUI event stream when active), and returns the
    /// pre-turn summary header.  Validation errors return their own message.
    ///
    /// For `traits`: returns `format_traits_listing(catalogue)`.
    ///
    /// For `install`: returns the hub install report.
    pub(crate) fn run_agent_command(
        &mut self,
        subcommand: AgentSubcommand,
    ) -> Result<String, Box<dyn std::error::Error>> {
        match subcommand {
            AgentSubcommand::Traits => {
                let catalogue = bundled_catalogue();
                Ok(format_traits_listing(catalogue))
            }
            AgentSubcommand::Compose { traits, task } => {
                if traits.is_empty() {
                    return Ok(format_agent_compose_empty_traits_usage());
                }
                if task.trim().is_empty() {
                    return Ok(format_agent_compose_empty_task_usage(&traits));
                }

                let catalogue = bundled_catalogue();
                let trait_refs: Vec<&str> = traits.iter().map(String::as_str).collect();

                match compose_agent(catalogue, &trait_refs, &task) {
                    Err(err) => Ok(format_agent_compose_error(&err)),
                    Ok(composed) => {
                        let header = format_agent_compose_summary(&composed);

                        // Rebuild runtime with composed system prompt prepended,
                        // run one turn, then restore — same pattern as /fast mode.
                        let original_system_prompt = self.system_prompt.clone();
                        let mut composed_system_prompt: Vec<runtime::PromptSection> = vec![
                            runtime::PromptSection::new(
                                runtime::PromptSectionKind::Custom,
                                composed.prompt.clone(),
                            ),
                        ];
                        composed_system_prompt.extend(original_system_prompt.iter().cloned());

                        let current_session = self.active_runtime().session().clone();
                        self.install_active_runtime(build_runtime_with_tui_slot(
                            current_session,
                            self.model.clone(),
                            composed_system_prompt,
                            true,
                            true,
                            self.allowed_tools.clone(),
                            self.permission_mode,
                            None,
                            self.active_tui_slot(),
                            self.agent_manager.clone(),
                        )?);

                        let turn_result = self.run_turn(&task);

                        // Restore original system prompt regardless of outcome.
                        let restore_result = build_runtime_with_tui_slot(
                            self.active_runtime().session().clone(),
                            self.model.clone(),
                            original_system_prompt.clone(),
                            true,
                            true,
                            self.allowed_tools.clone(),
                            self.permission_mode,
                            None,
                            self.active_tui_slot(),
                            self.agent_manager.clone(),
                        );
                        self.system_prompt = original_system_prompt;
                        if let Ok(restored) = restore_result {
                            self.install_active_runtime(restored);
                        }

                        match turn_result {
                            Ok(()) => Ok(header),
                            Err(e) => Ok(format!("{header}\nagent compose turn failed: {e}")),
                        }
                    }
                }
            }
            AgentSubcommand::Install { slug } => {
                Ok(self.run_hub_install_typed(&slug, Some("agent"), false))
            }
        }
    }

    // ── /share (REPL path) ────────────────────────────────────────────────────

    /// `/share` in batch/print REPL mode (non-TUI). TUI path has dedicated
    /// handling in `handle_repl_command_tui` to access the active tab.
    pub(crate) fn run_share_command_repl(&mut self, action: Option<&str>) -> String {
        match action.unwrap_or("").trim() {
            "stop" => {
                // CLI REPL uses tab_id "0" for the single implicit session.
                let synthetic = crate::tui::state::Tab {
                    id: 0,
                    name: "REPL".to_string(),
                    log: Vec::new(),
                    model: self.model.clone(),
                    session_id: self.session_id().to_string(),
                    pending_text: String::new(),
                    scroll: 0,
                    input: String::new(),
                    input_placeholders: Vec::new(),
                    pending_paste_blocks: Vec::new(),
                    cursor: 0,
                    history: Vec::new(),
                    history_idx: None,
                    history_backup: None,
                    think_label: String::new(),
                    think_start: None,
                    think_frame: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                    session_start: std::time::Instant::now(),
                    completion: Default::default(),
                    has_unread: false,
                    branches: Vec::new(),
                    active_branch: 0,
                    last_snapshot: None,
                    log_len_at_snapshot: None,
                    scrollback: crate::tui::scrollback::ScrollbackBuffer::new(),
                    scrollback_pending_lines: 0,
                    scrollback_state: crate::tui::scrollback::ScrollbackState::live(),
                    transcript_verbose: false,
                    ssh: None,
                    has_runtime: true,
                    cancel_token: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    message_queue: std::collections::VecDeque::new(),
                    in_flight: false,
                    tui_layout: crate::tui::state::Tab::load_default_layout(),
                    layout_local: crate::tui::layouts::LayoutLocalState::Classic,
                };
                self.share_manager.stop_share(&synthetic)
            }
            "list" => self.share_manager.list_shares(),
            // No subcommand → share the current REPL session.
            _ => {
                // Build share messages from the session (user + assistant only).
                let messages: Vec<runtime::ShareMessage> = self
                    .active_runtime()
                    .session()
                    .messages
                    .iter()
                    .filter_map(|msg| {
                        use runtime::MessageRole;
                        let role = match msg.role {
                            MessageRole::User => "user",
                            MessageRole::Assistant => "assistant",
                            _ => return None,
                        };
                        // Collect text content blocks only.
                        let content: String = msg
                            .blocks
                            .iter()
                            .filter_map(|b| {
                                if let runtime::ContentBlock::Text { text } = b {
                                    Some(text.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        if content.is_empty() {
                            return None;
                        }
                        Some(runtime::ShareMessage {
                            role: role.to_string(),
                            content,
                        })
                    })
                    .collect();

                let snapshot =
                    runtime::ShareSnapshot::build("REPL", &self.model, messages);

                // In the REPL there are no LogEntry items, so call the
                // BlockingShareClient directly (bypassing share_tab's log extraction).
                let client = match tokio::runtime::Handle::try_current() {
                    Ok(handle) => runtime::BlockingShareClient::default_client(handle),
                    Err(_) => match tokio::runtime::Runtime::new() {
                        Ok(rt) => {
                            runtime::BlockingShareClient::default_client(rt.handle().clone())
                        }
                        Err(e) => return format!("Share: could not start async runtime: {e}"),
                    },
                };

                match client.create_share("0", "REPL", snapshot, 86_400) {
                    Ok(share) => {
                        let url = share.url.clone();
                        let expires = share.expires_in_display();
                        self.share_manager.insert_active_share("0".to_string(), share);
                        format!("Shared at {url} (expires in {expires})")
                    }
                    Err(runtime::ShareError::RateLimitExceeded) => {
                        "Rate limit: 10 shares/hour. Try again later.".to_string()
                    }
                    Err(runtime::ShareError::RelayNotFound) => {
                        "Share is temporarily unavailable (relay endpoint not yet deployed)."
                            .to_string()
                    }
                    Err(e) => format!("Share unavailable (relay unreachable): {e}"),
                }
            }
        }
    }

    // ── /hub ──────────────────────────────────────────────────────────────────

    pub(crate) fn run_hub_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();

        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /hub                     Show top packages by category",
                "  /hub search <query>      Search all packages",
                "  /hub skills              Top skills",
                "  /hub plugins             Top plugins",
                "  /hub agents              Top agents",
                "  /hub themes              Top themes",
                "  /hub install <name>      Download and install a package",
                "  /hub info <name>         Show package details",
            ]
            .join("\n");
        }

        let hub_url = self.anvil_config_str("anvilhub_url", "https://anvilhub.culpur.net");

        let client = match tokio::runtime::Handle::try_current() {
            Ok(handle) => BlockingHubClient::new(&hub_url, handle),
            Err(_) => match tokio::runtime::Runtime::new() {
                Ok(rt) => BlockingHubClient::new(&hub_url, rt.handle().clone()),
                Err(e) => return format!("hub: could not start async runtime: {e}"),
            },
        };

        if let Some(query) = args.strip_prefix("search ").map(str::trim) {
            if query.is_empty() {
                return "Usage: /hub search <query>".to_string();
            }
            return match client.search(query, None) {
                Ok(pkgs) if pkgs.is_empty() => format!("No results for \"{query}\"."),
                Ok(pkgs) => format_package_list(&format!("Search results for \"{query}\""), &pkgs),
                Err(e) => format!("hub search: {e}"),
            };
        }

        if let Some(name) = args.strip_prefix("info ").map(str::trim) {
            if name.is_empty() {
                return "Usage: /hub info <name>".to_string();
            }
            return match client.get_package(name) {
                Ok(pkg) => format_package_detail(&pkg),
                Err(e) => format!("hub info: {e}"),
            };
        }

        if let Some(rest) = args.strip_prefix("install ").map(str::trim) {
            if rest.is_empty() {
                return "Usage: /hub install <name> [--allow-unverified]".to_string();
            }
            // Parse --allow-unverified flag from the remainder.
            let allow_unverified = rest.contains("--allow-unverified");
            let name = rest.replace("--allow-unverified", "").trim().to_string();
            if name.is_empty() {
                return "Usage: /hub install <name> [--allow-unverified]".to_string();
            }
            return self.run_hub_install_typed(&name, None, allow_unverified);
        }

        if let Some(name) = args.strip_prefix("status ").map(str::trim) {
            if name.is_empty() {
                return "Usage: /hub status <name>".to_string();
            }
            return match client.get_package(name) {
                Ok(pkg) => runtime::hub::format_package_status(&pkg),
                Err(e) => format!("hub status: {e}"),
            };
        }

        match args {
            "skills" | "plugins" | "agents" | "themes" => {
                let pkg_type = args.trim_end_matches('s');
                let label = args;
                match client.top_packages(pkg_type, 10) {
                    Ok(pkgs) if pkgs.is_empty() => format!("No {label} found."),
                    Ok(pkgs) => format_package_list(&format!("Top {label} on AnvilHub"), &pkgs),
                    Err(e) => format!("hub {args}: {e}"),
                }
            }
            _ => {
                // Default: top 5 of each category
                let mut out = String::from("AnvilHub — Top Packages\n");
                for (t, label) in &[
                    ("skill", "Skills"),
                    ("plugin", "Plugins"),
                    ("agent", "Agents"),
                    ("theme", "Themes"),
                ] {
                    match client.top_packages(t, 5) {
                        Ok(pkgs) => out.push_str(&format_package_list(&format!("\n{label}"), &pkgs)),
                        Err(e) => { let _ = write!(out, "\n{label}\n  (error: {e})\n"); }
                    }
                }
                out.push_str("\nRun /hub <category> for more, or /hub install <name> to install.");
                out
            }
        }
    }

    // ── Type-specific install helper ──────────────────────────────────────────
    //
    // Used by `/hub install`, `/skill install`, `/agent install`,
    // `/theme install`, and `/plugin install`.  When `expected_type` is `Some`,
    // the helper refuses to install a package whose `pkg_type` does not match
    // (so `/skill install <theme-slug>` is rejected before any download).
    //
    // Honours the AnvilHub trust gate (REVOKED is hard-blocked; unverified
    // packages require `allow_unverified` or `hub.require_verified=false`).
    // Emits `anvil.hub_package_install` OTel events for every outcome.
    pub(crate) fn run_hub_install_typed(
        &self,
        slug: &str,
        expected_type: Option<&str>,
        allow_unverified: bool,
    ) -> String {
        let slug_trim = slug.trim();
        if slug_trim.is_empty() {
            let label = expected_type.unwrap_or("name");
            return format!("Usage: /{label} install <slug>");
        }
        let hub_url = self.anvil_config_str("anvilhub_url", "https://anvilhub.culpur.net");
        let client = match tokio::runtime::Handle::try_current() {
            Ok(handle) => BlockingHubClient::new(&hub_url, handle),
            Err(_) => match tokio::runtime::Runtime::new() {
                Ok(rt) => BlockingHubClient::new(&hub_url, rt.handle().clone()),
                Err(e) => {
                    runtime::otel::hub_package_install(
                        expected_type.unwrap_or("unknown"),
                        slug_trim,
                        "error",
                    );
                    return format!("hub install: could not start async runtime: {e}");
                }
            },
        };
        let require_verified = runtime::ConfigLoader::default_for(
            std::env::current_dir().unwrap_or_default(),
        )
        .load()
        .map(|cfg| cfg.hub().require_verified())
        .unwrap_or(false);
        let pkg = match client.get_package(slug_trim) {
            Ok(p) => p,
            Err(e) => {
                runtime::otel::hub_package_install(
                    expected_type.unwrap_or("unknown"),
                    slug_trim,
                    "error",
                );
                return format!("hub install: {e}");
            }
        };
        if let Some(expected) = expected_type {
            if !pkg.pkg_type.eq_ignore_ascii_case(expected) {
                runtime::otel::hub_package_install(expected, slug_trim, "type-mismatch");
                return format!(
                    "Type mismatch: '{}' is a {} (not a {}). Try /{} install {}, or /hub install {} for a type-agnostic install.",
                    pkg.name, pkg.pkg_type, expected, pkg.pkg_type, slug_trim, slug_trim,
                );
            }
        }
        let install_dir = anvil_home_dir();
        match client.install(&pkg, &install_dir, require_verified, allow_unverified) {
            Ok(dest) => {
                runtime::otel::hub_package_install(&pkg.pkg_type, slug_trim, "ok");
                format!(
                    "Installed {} v{} ({}) to {}",
                    pkg.name,
                    pkg.version,
                    pkg.pkg_type,
                    dest.display()
                )
            }
            Err(runtime::HubError::Revoked(msg)) => {
                runtime::otel::hub_package_install(&pkg.pkg_type, slug_trim, "revoked");
                format!("hub install: {msg}")
            }
            Err(runtime::HubError::Unverified(msg)) => {
                runtime::otel::hub_package_install(&pkg.pkg_type, slug_trim, "unverified");
                format!("hub install: {msg}")
            }
            Err(e) => {
                runtime::otel::hub_package_install(&pkg.pkg_type, slug_trim, "error");
                format!("hub install: {e}")
            }
        }
    }
}
