//! Configure command handler and data-builder for `impl LiveCli`.
//!
//! These remain `impl LiveCli` methods but are in a separate file to reduce
//! the size of `main.rs`.

#![allow(clippy::too_many_lines)]

use std::fs;


use crate::tui::{ConfigureAction, ConfigureData};
use crate::{
    anvil_pinned_path, dirs_next_home, format_number, load_pinned_paths, parse_token_count,
    LiveCli,
};

impl LiveCli {
    pub(crate) fn load_anvil_ui_config() -> serde_json::Map<String, serde_json::Value> {
        let Some(home) = dirs_next_home() else {
            return serde_json::Map::new();
        };
        let path = home.join(".anvil").join("config.json");
        if !path.exists() {
            return serde_json::Map::new();
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            return serde_json::Map::new();
        };
        match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        }
    }

    pub(crate) fn save_anvil_ui_config_key(key: &str, value: serde_json::Value) -> String {
        let Some(home) = dirs_next_home() else {
            return "Error: could not determine home directory.".to_string();
        };
        let anvil_dir = home.join(".anvil");
        if let Err(e) = fs::create_dir_all(&anvil_dir) {
            return format!("Error creating ~/.anvil: {e}");
        }
        let path = anvil_dir.join("config.json");
        let mut map = Self::load_anvil_ui_config();
        map.insert(key.to_string(), value.clone());
        let serialised = serde_json::to_string_pretty(&serde_json::Value::Object(map))
            .unwrap_or_else(|_| "{}".to_string());
        match fs::write(&path, serialised) {
            Ok(()) => format!("Saved {key} = {value} to ~/.anvil/config.json"),
            Err(e) => format!("Error writing config: {e}"),
        }
    }

    pub(crate) fn run_configure_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let cfg = Self::load_anvil_ui_config();

        // Helper: read a string key from config, with a fallback.
        let cfg_str = |key: &str, fallback: &str| -> String {
            cfg.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or(fallback)
                .to_string()
        };
        let cfg_bool = |key: &str, fallback: bool| -> bool {
            cfg.get(key)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(fallback)
        };
        let cfg_u64 = |key: &str, fallback: u64| -> u64 {
            cfg.get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(fallback)
        };

        // Parse first word as section.
        let mut parts = args.splitn(3, ' ');
        let section = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();
        let value = parts.next().unwrap_or("").trim();

        match section {
            // ── Main menu ──────────────────────────────────────────────────
            "" => {
                [
                    "Anvil Configuration",
                    "",
                    "  /configure providers    Providers & authentication",
                    "  /configure models       Models & defaults",
                    "  /configure context      Context & memory",
                    "  /configure search       Search providers",
                    "  /configure permissions  Permissions & security",
                    "  /configure display      Display & interface",
                    "  /configure integrations Integrations",
                    "",
                    "Append a sub-command for details, e.g.:",
                    "  /configure models default claude-sonnet-4-6",
                    "  /configure search tavily <api-key>",
                    "  /configure display vim on",
                ]
                .join("\n")
            }

            // ── Providers & authentication ────────────────────────────────
            "providers" => {
                // Check whether creds are present for each provider.
                let anthropic_key = std::env::var("ANTHROPIC_API_KEY").ok().filter(|s| !s.is_empty());
                let anthropic_oauth = runtime::load_oauth_credentials().ok().flatten().is_some();
                let anthropic_status = if anthropic_oauth {
                    "[✓ OAuth]"
                } else if anthropic_key.is_some() {
                    "[✓ API key]"
                } else {
                    "[✗ not configured]"
                };

                let openai_key = std::env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty());
                let openai_status = if openai_key.is_some() { "[✓ API key]" } else { "[✗ not configured]" };

                let ollama_host = std::env::var("OLLAMA_HOST")
                    .unwrap_or_else(|_| "http://localhost:11434".to_string());
                let ollama_alive = std::process::Command::new("curl")
                    .args(["-sf", "--max-time", "1", &format!("{ollama_host}/api/tags")])
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                let ollama_status = if ollama_alive { "[✓ reachable]" } else { "[✗ not reachable]" };

                let xai_key = std::env::var("XAI_API_KEY").ok().filter(|s| !s.is_empty());
                let xai_status = if xai_key.is_some() { "[✓ API key]" } else { "[✗ not configured]" };

                match rest {
                    "" => {
                        [
                            "Providers & Authentication",
                            "",
                            &format!("  Anthropic   {anthropic_status}"),
                            &format!("  OpenAI      {openai_status}"),
                            &format!("  Ollama      {ollama_status}  ({ollama_host})"),
                            &format!("  xAI         {xai_status}"),
                            "",
                            "To configure:",
                            "  /configure providers anthropic   OAuth login (browser)",
                            "  /configure providers openai      Set OPENAI_API_KEY",
                            "  /configure providers ollama      Set Ollama host URL",
                            "  /configure providers xai         Set XAI_API_KEY",
                            "",
                            "Or use /login [anthropic|openai|ollama|xai]",
                        ]
                        .join("\n")
                    }
                    "anthropic" => {
                        "To authenticate with Anthropic, run:\n  /login anthropic\n\n\
                         This starts an OAuth browser flow and stores credentials in ~/.anvil/oauth.json.\n\
                         Alternatively, set ANTHROPIC_API_KEY in your shell environment."
                            .to_string()
                    }
                    "openai" => {
                        if value.starts_with("sk-") {
                            Self::save_anvil_ui_config_key("openai_api_key", serde_json::Value::String(value.to_string()))
                        } else {
                            "To configure OpenAI:\n  /configure providers openai <api-key>\n\n\
                             Or set OPENAI_API_KEY in your shell environment.\n\
                             Get an API key at https://platform.openai.com/api-keys"
                                .to_string()
                        }
                    }
                    "ollama" => {
                        if value.is_empty() {
                            format!(
                                "Ollama host: {ollama_host}\n\n\
                                 To change: /configure providers ollama <url>\n\
                                 Or set OLLAMA_HOST in your shell environment.\n\
                                 Default:   http://localhost:11434\n\n\
                                 Status:    {ollama_status}"
                            )
                        } else {
                            Self::save_anvil_ui_config_key("ollama_host", serde_json::Value::String(value.to_string()))
                        }
                    }
                    "xai" => {
                        if value.starts_with("xai-") || (!value.is_empty() && !value.starts_with('/')) {
                            Self::save_anvil_ui_config_key("xai_api_key", serde_json::Value::String(value.to_string()))
                        } else {
                            "To configure xAI:\n  /configure providers xai <api-key>\n\n\
                             Or set XAI_API_KEY in your shell environment.\n\
                             Get an API key at https://console.x.ai"
                                .to_string()
                        }
                    }
                    other => format!("Unknown provider: {other}\nAvailable: anthropic, openai, ollama, xai"),
                }
            }

            // ── Models & defaults ─────────────────────────────────────────
            "models" => {
                let default_model = cfg_str("default_model", &self.model);
                let image_model = cfg_str("image_model", "gpt-image-1.5");

                // Load failover chain for display.
                let chain = api::FailoverChain::from_config_file();
                let chain_lines = chain.format_status();

                match rest {
                    "" => {
                        let mut lines = vec![
                            "Models & Defaults".to_string(),
                            String::new(),
                            format!("  Default model:    {default_model}"),
                            format!("  Image model:      {image_model}"),
                            format!("  Active model:     {}", self.model),
                            String::new(),
                            "Failover chain:".to_string(),
                        ];
                        for line in chain_lines.lines() {
                            lines.push(format!("  {line}"));
                        }
                        lines.push(String::new());
                        lines.push("To change:".to_string());
                        lines.push("  /configure models default <model>   Set startup default".to_string());
                        lines.push("  /configure models image <model>     Set image generation model".to_string());
                        lines.push("  /model <name>                       Switch active model now".to_string());
                        lines.push("  /failover add <model>               Add to failover chain".to_string());
                        lines.join("\n")
                    }
                    "default" => {
                        if value.is_empty() {
                            format!("Current default model: {default_model}\n\nUsage: /configure models default <model>")
                        } else {
                            Self::save_anvil_ui_config_key("default_model", serde_json::Value::String(value.to_string()))
                        }
                    }
                    "image" => {
                        if value.is_empty() {
                            format!("Current image model: {image_model}\n\nUsage: /configure models image <model>")
                        } else {
                            Self::save_anvil_ui_config_key("image_model", serde_json::Value::String(value.to_string()))
                        }
                    }
                    other => format!("Unknown sub-command: {other}\nUsage: /configure models [default|image] [<value>]"),
                }
            }

            // ── Context & memory ──────────────────────────────────────────
            "context" => {
                let context_size = cfg_u64("context_size", 1_000_000);
                let compact_threshold = cfg_u64("compact_threshold", 85);
                let qmd_enabled = cfg_bool("qmd_enabled", true);
                let history_enabled = cfg_bool("history_enabled", true);

                // Pinned files count.
                let pinned_count = anvil_pinned_path()
                    .ok()
                    .and_then(|p| load_pinned_paths(&p).ok())
                    .map_or(0, |v| v.len());

                // QMD status.
                let qmd_status = if !self.qmd.is_enabled() {
                    "disabled (binary not found)".to_string()
                } else if !qmd_enabled {
                    "disabled (config)".to_string()
                } else {
                    match self.qmd.status() {
                        Some(s) => format!("enabled ({} docs, {} vectors)", s.total_docs, s.total_vectors),
                        None => "enabled (status unavailable)".to_string(),
                    }
                };

                // History archive count.
                let archive_count = self.history_archiver.list_archives().len();

                match rest {
                    "" => [
                        "Context & Memory",
                        "",
                        &format!("  Context size:      {:>13} tokens", format_number(context_size)),
                        &format!("  Auto-compact:      {compact_threshold}% threshold"),
                        &format!("  QMD integration:   {qmd_status}"),
                        &format!("  History archival:  {} ({} archives in ~/.anvil/history/)", if history_enabled { "enabled" } else { "disabled" }, archive_count),
                        &format!("  Pinned files:      {pinned_count}"),
                        "",
                        "To change:",
                        "  /configure context size 2M          Set context size (e.g. 200K, 1M, 2M)",
                        "  /configure context threshold 90     Set auto-compact threshold (%)",
                        "  /configure context qmd off          Disable QMD integration",
                        "  /configure context history off      Disable history archival",
                        "  /pin <path>                         Pin a file to always-in-context",
                    ]
                    .join("\n"),
                    "size" => {
                        if value.is_empty() {
                            format!("Current context size: {} tokens\n\nUsage: /configure context size <n>  (e.g. 200K, 1M, 2M)", format_number(context_size))
                        } else {
                            let parsed = parse_token_count(value);
                            match parsed {
                                Some(n) => Self::save_anvil_ui_config_key("context_size", serde_json::Value::Number(serde_json::Number::from(n))),
                                None => format!("Invalid size: {value}\nExamples: 200000, 200K, 1M, 2M"),
                            }
                        }
                    }
                    "threshold" => {
                        if value.is_empty() {
                            format!("Current compact threshold: {compact_threshold}%\n\nUsage: /configure context threshold <1-100>")
                        } else {
                            match value.parse::<u64>() {
                                Ok(n) if (1..=100).contains(&n) => {
                                    Self::save_anvil_ui_config_key("compact_threshold", serde_json::Value::Number(n.into()))
                                }
                                _ => format!("Invalid threshold: {value}\nMust be a number between 1 and 100"),
                            }
                        }
                    }
                    "qmd" => match value {
                        "on" | "enable" | "enabled" | "true" | "1" => {
                            Self::save_anvil_ui_config_key("qmd_enabled", serde_json::Value::Bool(true))
                        }
                        "off" | "disable" | "disabled" | "false" | "0" => {
                            Self::save_anvil_ui_config_key("qmd_enabled", serde_json::Value::Bool(false))
                        }
                        "" => format!("QMD: {qmd_status}\n\nUsage: /configure context qmd [on|off]"),
                        other => format!("Invalid value: {other}\nUsage: /configure context qmd [on|off]"),
                    },
                    "history" => match value {
                        "on" | "enable" | "enabled" | "true" | "1" => {
                            Self::save_anvil_ui_config_key("history_enabled", serde_json::Value::Bool(true))
                        }
                        "off" | "disable" | "disabled" | "false" | "0" => {
                            Self::save_anvil_ui_config_key("history_enabled", serde_json::Value::Bool(false))
                        }
                        "" => format!("History archival: {}\n\nUsage: /configure context history [on|off]", if history_enabled { "enabled" } else { "disabled" }),
                        other => format!("Invalid value: {other}\nUsage: /configure context history [on|off]"),
                    },
                    other => format!("Unknown sub-command: {other}\nUsage: /configure context [size|threshold|qmd|history]"),
                }
            }

            // ── Search providers ──────────────────────────────────────────
            "search" => {
                let engine = runtime::SearchEngine::from_env_and_config();
                let default_provider = engine.default_provider().to_string();
                let providers = engine.list_providers();

                let check = |name: &str| -> &'static str {
                    providers
                        .iter()
                        .find(|(n, _, _)| n == name)
                        .map_or("[✗]", |(_, _, has_creds)| if *has_creds { "[✓]" } else { "[✗ no key]" })
                };
                let searxng_url = std::env::var("SEARXNG_URL")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "not set".to_string());

                match rest {
                    "" => [
                        "Search Providers",
                        "",
                        &format!("  Default provider:  {default_provider}"),
                        "",
                        "  Providers:",
                        "    DuckDuckGo   [✓ free, no key]",
                        &format!("    Tavily       {}  /configure search tavily <key>", check("tavily")),
                        &format!("    Brave        {}  /configure search brave <key>", check("brave")),
                        &format!("    SearXNG      [✓ {searxng_url}]  /configure search searxng <url>"),
                        &format!("    Exa          {}  /configure search exa <key>", check("exa")),
                        &format!("    Perplexity   {}  /configure search perplexity <key>", check("perplexity")),
                        &format!("    Google       {}  /configure search google <key> <cx>", check("google")),
                        &format!("    Bing         {}  /configure search bing <key>", check("bing")),
                        "",
                        "  To set default:  /configure search default <provider>",
                    ]
                    .join("\n"),
                    "default" => {
                        if value.is_empty() {
                            format!("Current default search provider: {default_provider}\n\nUsage: /configure search default <provider>")
                        } else {
                            Self::save_anvil_ui_config_key("default_search", serde_json::Value::String(value.to_string()))
                        }
                    }
                    "tavily" => {
                        if value.is_empty() {
                            format!("Tavily: {}\n\nUsage: /configure search tavily <api-key>\nGet a key at https://tavily.com", check("tavily"))
                        } else {
                            Self::save_anvil_ui_config_key("tavily_api_key", serde_json::Value::String(value.to_string()))
                        }
                    }
                    "brave" => {
                        if value.is_empty() {
                            format!("Brave Search: {}\n\nUsage: /configure search brave <api-key>\nGet a key at https://brave.com/search/api", check("brave"))
                        } else {
                            Self::save_anvil_ui_config_key("brave_search_api_key", serde_json::Value::String(value.to_string()))
                        }
                    }
                    "exa" => {
                        if value.is_empty() {
                            format!("Exa: {}\n\nUsage: /configure search exa <api-key>\nGet a key at https://exa.ai", check("exa"))
                        } else {
                            Self::save_anvil_ui_config_key("exa_api_key", serde_json::Value::String(value.to_string()))
                        }
                    }
                    "perplexity" => {
                        if value.is_empty() {
                            format!("Perplexity: {}\n\nUsage: /configure search perplexity <api-key>\nGet a key at https://www.perplexity.ai/settings/api", check("perplexity"))
                        } else {
                            Self::save_anvil_ui_config_key("perplexity_api_key", serde_json::Value::String(value.to_string()))
                        }
                    }
                    "searxng" => {
                        if value.is_empty() {
                            format!("SearXNG URL: {searxng_url}\n\nUsage: /configure search searxng <url>\nExample: /configure search searxng https://searx.be")
                        } else {
                            Self::save_anvil_ui_config_key("searxng_url", serde_json::Value::String(value.to_string()))
                        }
                    }
                    "google" => {
                        // Accepts "<key> <cx>" or just "<key>"
                        let mut gparts = value.splitn(2, ' ');
                        let gkey = gparts.next().unwrap_or("").trim();
                        let gcx = gparts.next().unwrap_or("").trim();
                        if gkey.is_empty() {
                            format!("Google Search: {}\n\nUsage: /configure search google <api-key> <cx>\nGet credentials at https://developers.google.com/custom-search/v1/overview", check("google"))
                        } else {
                            let mut result = Self::save_anvil_ui_config_key("google_search_api_key", serde_json::Value::String(gkey.to_string()));
                            if !gcx.is_empty() {
                                result.push('\n');
                                result.push_str(&Self::save_anvil_ui_config_key("google_search_cx", serde_json::Value::String(gcx.to_string())));
                            }
                            result
                        }
                    }
                    "bing" => {
                        if value.is_empty() {
                            format!("Bing Search: {}\n\nUsage: /configure search bing <api-key>\nGet a key at https://azure.microsoft.com/en-us/products/bing-search", check("bing"))
                        } else {
                            Self::save_anvil_ui_config_key("bing_search_api_key", serde_json::Value::String(value.to_string()))
                        }
                    }
                    other => format!("Unknown provider: {other}\nAvailable: default, tavily, brave, exa, perplexity, searxng, google, bing"),
                }
            }

            // ── Permissions & security ────────────────────────────────────
            "permissions" => {
                let mode = self.permission_mode.as_str();
                match rest {
                    "" => [
                        "Permissions & Security",
                        "",
                        &format!("  Mode:     {mode}"),
                        "",
                        "  Modes:",
                        "    read-only           Read files only, no writes or shell commands",
                        "    workspace-write     Read + write workspace files, no shell commands",
                        "    danger-full-access  Full tool access including shell (default)",
                        "",
                        "To change:",
                        "  /configure permissions read-only",
                        "  /configure permissions workspace-write",
                        "  /configure permissions danger-full-access",
                        "  /permissions <mode>  (same effect, immediate)",
                    ]
                    .join("\n"),
                    "read-only" | "workspace-write" | "danger-full-access" => {
                        format!(
                            "To switch permissions now, use:\n  /permissions {rest}\n\n\
                             To make this the default, add ANVIL_PERMISSION_MODE={rest} to your shell environment."
                        )
                    }
                    other => format!(
                        "Unknown mode: {other}\nAvailable: read-only, workspace-write, danger-full-access"
                    ),
                }
            }

            // ── Display & interface ───────────────────────────────────────
            "display" => {
                let vim_mode = self.vim_mode;
                let chat_mode = self.chat_mode;
                let tab_forward = cfg_str("tab_key_forward", "Ctrl+]");
                let tab_back = cfg_str("tab_key_back", "Ctrl+[");

                match rest {
                    "" => [
                        "Display & Interface",
                        "",
                        &format!("  Vim mode:    {}", if vim_mode { "on" } else { "off" }),
                        &format!("  Chat mode:   {}", if chat_mode { "on  (tools disabled)" } else { "off" }),
                        &format!("  Tab keys:    {tab_forward} / {tab_back}"),
                        "",
                        "To change:",
                        "  /configure display vim on|off    Toggle vim keybindings",
                        "  /configure display chat on|off   Toggle chat-only mode (disables tools)",
                        "  /vim                             Toggle vim keybindings immediately",
                        "  /chat                            Toggle chat mode immediately",
                    ]
                    .join("\n"),
                    "vim" => match value {
                        "on" | "enable" | "enabled" | "true" | "1" => {
                            let saved = Self::save_anvil_ui_config_key("vim_mode", serde_json::Value::Bool(true));
                            format!("{saved}\nNote: use /vim to toggle immediately in the current session.")
                        }
                        "off" | "disable" | "disabled" | "false" | "0" => {
                            let saved = Self::save_anvil_ui_config_key("vim_mode", serde_json::Value::Bool(false));
                            format!("{saved}\nNote: use /vim to toggle immediately in the current session.")
                        }
                        "" => format!(
                            "Vim mode: {}\n\nUsage: /configure display vim [on|off]\nOr use /vim to toggle immediately.",
                            if vim_mode { "on" } else { "off" }
                        ),
                        other => format!("Invalid value: {other}\nUsage: /configure display vim [on|off]"),
                    },
                    "chat" => match value {
                        "on" | "enable" | "enabled" | "true" | "1" => {
                            let saved = Self::save_anvil_ui_config_key("chat_mode", serde_json::Value::Bool(true));
                            format!("{saved}\nNote: use /chat to toggle immediately in the current session.")
                        }
                        "off" | "disable" | "disabled" | "false" | "0" => {
                            let saved = Self::save_anvil_ui_config_key("chat_mode", serde_json::Value::Bool(false));
                            format!("{saved}\nNote: use /chat to toggle immediately in the current session.")
                        }
                        "" => format!(
                            "Chat mode: {}\n\nUsage: /configure display chat [on|off]\nOr use /chat to toggle immediately.",
                            if chat_mode { "on" } else { "off" }
                        ),
                        other => format!("Invalid value: {other}\nUsage: /configure display chat [on|off]"),
                    },
                    other => format!("Unknown sub-command: {other}\nUsage: /configure display [vim|chat]"),
                }
            }

            // ── Integrations ──────────────────────────────────────────────
            "integrations" => {
                let anvilhub_url = cfg_str("anvilhub_url", "https://anvilhub.culpur.net");
                let wp_url = std::env::var("WP_URL")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .or_else(|| cfg.get("wp_url").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string));
                let wp_user = std::env::var("WP_USER")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .or_else(|| cfg.get("wp_user").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string));
                let github_token = std::env::var("GITHUB_TOKEN")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .or_else(|| std::env::var("GH_TOKEN").ok().filter(|s| !s.is_empty()));

                let anvilhub_status = "[connected]";
                let wp_status = if wp_url.is_some() && wp_user.is_some() { "[configured]" } else { "[not configured]" };
                let gh_status = if github_token.is_some() { "[✓ token set]" } else { "[✗ not configured]" };

                match rest {
                    "" => [
                        "Integrations",
                        "",
                        &format!("  AnvilHub:    {anvilhub_url}  {anvilhub_status}"),
                        &format!("  WordPress:   {}  {wp_status}", wp_url.as_deref().unwrap_or("not configured")),
                        &format!("  GitHub:      {gh_status}"),
                        "",
                        "To configure:",
                        "  /configure integrations anvilhub <url>",
                        "  /configure integrations wp <url> <user>",
                        "  /configure integrations github <token>",
                    ]
                    .join("\n"),
                    "anvilhub" => {
                        if value.is_empty() {
                            format!("AnvilHub URL: {anvilhub_url}\n\nUsage: /configure integrations anvilhub <url>")
                        } else {
                            Self::save_anvil_ui_config_key("anvilhub_url", serde_json::Value::String(value.to_string()))
                        }
                    }
                    "wp" | "wordpress" => {
                        // Accepts "<url> <user>" or just "<url>"
                        let mut wparts = value.splitn(2, ' ');
                        let wurl = wparts.next().unwrap_or("").trim();
                        let wuser = wparts.next().unwrap_or("").trim();
                        if wurl.is_empty() {
                            let current = match (&wp_url, &wp_user) {
                                (Some(u), Some(usr)) => format!("URL: {u}  User: {usr}"),
                                (Some(u), None) => format!("URL: {u}  User: (not set)"),
                                _ => "Not configured".to_string(),
                            };
                            format!(
                                "WordPress: {current}\n\n\
                                 Usage: /configure integrations wp <url> <user>\n\
                                 Set WP_APP_PASSWORD in your shell for the application password."
                            )
                        } else {
                            let mut result = Self::save_anvil_ui_config_key("wp_url", serde_json::Value::String(wurl.to_string()));
                            if !wuser.is_empty() {
                                result.push('\n');
                                result.push_str(&Self::save_anvil_ui_config_key("wp_user", serde_json::Value::String(wuser.to_string())));
                            }
                            result.push_str("\nNote: set WP_APP_PASSWORD in your environment for the application password.");
                            result
                        }
                    }
                    "github" | "gh" => {
                        if value.is_empty() {
                            format!("GitHub: {gh_status}\n\nUsage: /configure integrations github <token>\nOr set GITHUB_TOKEN in your environment.\nGet a token at https://github.com/settings/tokens")
                        } else {
                            let saved = Self::save_anvil_ui_config_key("github_token", serde_json::Value::String(value.to_string()));
                            format!("{saved}\nNote: also set GITHUB_TOKEN in your shell for tools that read from environment.")
                        }
                    }
                    other => format!("Unknown integration: {other}\nAvailable: anvilhub, wp, github"),
                }
            }

            // ── Unknown section ───────────────────────────────────────────
            other => {
                format!(
                    "Unknown section: {other}\n\n\
                     Available: providers, models, context, search, permissions, display, integrations\n\n\
                     Run /configure for the main menu."
                )
            }
        }
    }

    pub(crate) fn build_configure_data(&self) -> ConfigureData {
        let cfg = Self::load_anvil_ui_config();
        let cfg_str = |key: &str, fallback: &str| -> String {
            cfg.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or(fallback)
                .to_string()
        };
        let cfg_bool = |key: &str, fallback: bool| -> bool {
            cfg.get(key)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(fallback)
        };
        let cfg_u64 = |key: &str, fallback: u64| -> u64 {
            cfg.get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(fallback)
        };

        // Providers.
        let anthropic_key = std::env::var("ANTHROPIC_API_KEY").ok().filter(|s| !s.is_empty());
        let anthropic_oauth = runtime::load_oauth_credentials().ok().flatten().is_some();
        let anthropic_status = if anthropic_oauth {
            "✓ OAuth active".to_string()
        } else if anthropic_key.is_some() {
            "✓ API key".to_string()
        } else {
            "✗ not configured".to_string()
        };

        let openai_status = if std::env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()).is_some() {
            "✓ API key".to_string()
        } else {
            "✗ not configured".to_string()
        };

        let ollama_host = std::env::var("OLLAMA_HOST")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| cfg_str("ollama_host", "http://localhost:11434"));

        let ollama_alive = std::process::Command::new("curl")
            .args(["-sf", "--max-time", "1", &format!("{ollama_host}/api/tags")])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let ollama_status = if ollama_alive {
            "✓ reachable".to_string()
        } else {
            "✗ not reachable".to_string()
        };

        let xai_status = if std::env::var("XAI_API_KEY").ok().filter(|s| !s.is_empty()).is_some() {
            "✓ API key".to_string()
        } else {
            "✗ not configured".to_string()
        };

        // Models.
        let default_model = cfg_str("default_model", &self.model);
        let image_model = cfg_str("image_model", "gpt-image-1.5");
        let failover_chain = {
            let chain = api::FailoverChain::from_config_file();
            let mut models = Vec::new();
            let mut idx = 0;
            while let Some(m) = chain.model_at(idx) {
                models.push(m.to_string());
                idx += 1;
            }
            models
        };

        // Context.
        let context_size = cfg_u64("context_size", 1_000_000);
        let compact_threshold = cfg_u64("compact_threshold", 85) as u8;
        let qmd_enabled = cfg_bool("qmd_enabled", true);
        let qmd_status = if !self.qmd.is_enabled() {
            "disabled (binary not found)".to_string()
        } else if !qmd_enabled {
            "disabled (config)".to_string()
        } else {
            match self.qmd.status() {
                Some(s) => format!("enabled ({} docs, {} vectors)", s.total_docs, s.total_vectors),
                None => "enabled".to_string(),
            }
        };
        let history_count = self.history_archiver.list_archives().len();
        let pinned_count = anvil_pinned_path()
            .ok()
            .and_then(|p| load_pinned_paths(&p).ok())
            .map_or(0, |v| v.len());

        // Search.
        let engine = runtime::SearchEngine::from_env_and_config();
        let default_search = engine.default_provider().to_string();
        let search_providers = vec![
            ("Tavily".to_string(), true, std::env::var("TAVILY_API_KEY").ok().filter(|s| !s.is_empty()).is_some() || cfg.get("tavily_api_key").is_some()),
            ("Brave".to_string(), true, std::env::var("BRAVE_SEARCH_API_KEY").ok().filter(|s| !s.is_empty()).is_some() || cfg.get("brave_search_api_key").is_some()),
            ("SearXNG".to_string(), true, !cfg_str("searxng_url", "").is_empty()),
            ("Exa".to_string(), true, std::env::var("EXA_API_KEY").ok().filter(|s| !s.is_empty()).is_some() || cfg.get("exa_api_key").is_some()),
            ("Perplexity".to_string(), true, std::env::var("PERPLEXITY_API_KEY").ok().filter(|s| !s.is_empty()).is_some() || cfg.get("perplexity_api_key").is_some()),
        ];

        // Display.
        let vim_mode = cfg_bool("vim_mode", false);
        let chat_mode = cfg_bool("chat_mode", false);
        let permission_mode = self.permission_mode.as_str().to_string();

        // Integrations.
        let anvilhub_url = cfg_str("anvilhub_url", "");
        let wp_configured = cfg.get("wp_url").is_some() || std::env::var("WP_URL").ok().filter(|s| !s.is_empty()).is_some();
        let github_configured = std::env::var("GITHUB_TOKEN").ok().filter(|s| !s.is_empty()).is_some() || cfg.get("github_token").is_some();

        // Section 7: Language & Theme
        let language = cfg_str("language", "en");
        let active_theme = cfg_str("theme", "culpur-defense");

        // Section 8: Vault
        let vault_session_ttl = cfg_u64("vault_session_ttl", 1800);
        let vault_auto_lock = cfg_bool("vault_auto_lock", false);
        let vault_status = cfg_str("vault_status_display", "locked | 0 creds, 0 TOTP");

        // Section 9: Notifications
        let notify_platform = cfg_str("notify_platform", "desktop");
        let env_or_cfg = |env_key: &str, cfg_key: &str| -> String {
            std::env::var(env_key).ok().filter(|s| !s.is_empty())
                .unwrap_or_else(|| cfg_str(cfg_key, ""))
        };
        let notify_discord_webhook  = env_or_cfg("DISCORD_WEBHOOK_URL",   "notify_discord_webhook");
        let notify_slack_webhook    = env_or_cfg("SLACK_WEBHOOK_URL",      "notify_slack_webhook");
        let notify_telegram_token   = env_or_cfg("TELEGRAM_BOT_TOKEN",     "notify_telegram_token");
        let notify_whatsapp_url     = env_or_cfg("WHATSAPP_API_URL",       "notify_whatsapp_url");
        let notify_whatsapp_token   = env_or_cfg("WHATSAPP_TOKEN",         "notify_whatsapp_token");
        let notify_matrix_homeserver = env_or_cfg("MATRIX_HOMESERVER",     "notify_matrix_homeserver");
        let notify_matrix_token     = env_or_cfg("MATRIX_TOKEN",           "notify_matrix_token");
        let notify_signal_sender    = env_or_cfg("SIGNAL_SENDER",          "notify_signal_sender");
        let notify_signal_cli_path  = env_or_cfg("SIGNAL_CLI_PATH",        "notify_signal_cli_path");

        // Section 10: Failover
        let failover_cooldown = cfg_u64("failover_cooldown", 60);
        let failover_budget   = cfg_u64("failover_budget", 0);
        let failover_auto_recovery = cfg_bool("failover_auto_recovery", true);

        // Section 11: SSH
        let ssh_key_path     = env_or_cfg("ANVIL_SSH_KEY",     "ssh_key_path");
        let ssh_bastion_host = env_or_cfg("ANVIL_BASTION_HOST","ssh_bastion_host");
        let ssh_config_path  = cfg_str("ssh_config_path", "~/.ssh/config");

        // Section 12: Docker & K8s
        let docker_compose_file = env_or_cfg("COMPOSE_FILE",       "docker_compose_file");
        let docker_registry     = env_or_cfg("DOCKER_REGISTRY",    "docker_registry");
        let k8s_context         = env_or_cfg("KUBE_CONTEXT",       "k8s_context");
        let k8s_namespace       = env_or_cfg("KUBE_NAMESPACE",     "k8s_namespace");

        // Section 13: Database
        let db_url         = env_or_cfg("DATABASE_URL", "db_url");
        let db_schema_tool = cfg_str("db_schema_tool", "prisma");

        // Section 14: Memory & Archive
        let auto_save_memory        = cfg_bool("auto_save_memory", true);
        let archive_frequency       = cfg_u64("archive_frequency", 5);
        let archive_retention_days  = cfg_u64("archive_retention_days", 30);
        let memory_dir              = cfg_str("memory_dir", "");

        // Section 15: Plugins & Cron
        let plugin_search_paths  = cfg_str("plugin_search_paths", "");
        let auto_enable_plugins  = cfg_bool("auto_enable_plugins", false);
        let cron_enabled         = cfg_bool("cron_enabled", false);
        // Active cron jobs from config array (best-effort, empty if not present).
        let active_cron_jobs: Vec<String> = cfg.get("cron_jobs")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter()
                .filter_map(|item| item.as_str().map(std::string::ToString::to_string))
                .collect())
            .unwrap_or_default();

        ConfigureData {
            anthropic_status,
            openai_status,
            ollama_status,
            ollama_host,
            xai_status,
            current_model: self.model.clone(),
            default_model,
            image_model,
            failover_chain,
            context_size,
            compact_threshold,
            qmd_status,
            history_count,
            pinned_count,
            default_search,
            search_providers,
            vim_mode,
            chat_mode,
            permission_mode,
            screensaver_timeout_mins: cfg_u64("screensaver_timeout_mins", 15),
            screensaver_enabled: cfg_bool("screensaver_enabled", true),
            anvilhub_url,
            wp_configured,
            github_configured,
            // Section 7
            language,
            active_theme,
            // Section 8
            vault_session_ttl,
            vault_auto_lock,
            vault_status,
            // Section 9
            notify_platform,
            notify_discord_webhook,
            notify_slack_webhook,
            notify_telegram_token,
            notify_whatsapp_url,
            notify_whatsapp_token,
            notify_matrix_homeserver,
            notify_matrix_token,
            notify_signal_sender,
            notify_signal_cli_path,
            // Section 10
            failover_cooldown,
            failover_budget,
            failover_auto_recovery,
            // Section 11
            ssh_key_path,
            ssh_bastion_host,
            ssh_config_path,
            // Section 12
            docker_compose_file,
            docker_registry,
            k8s_context,
            k8s_namespace,
            // Section 13
            db_url,
            db_schema_tool,
            // Section 14
            auto_save_memory,
            archive_frequency,
            archive_retention_days,
            memory_dir,
            // Section 15
            plugin_search_paths,
            auto_enable_plugins,
            cron_enabled,
            active_cron_jobs,
        }
    }

    pub(crate) fn apply_configure_action(&mut self, action: ConfigureAction) -> String {
        match action {
            ConfigureAction::RefreshAnthropicOAuth => {
                // Delegate to the existing /login flow (leaves alternate screen temporarily).
                self.run_inline_login(Some("anthropic"))
            }
            ConfigureAction::SetApiKey { provider, key } => {
                let config_key = match provider.as_str() {
                    "anthropic" => "anthropic_api_key",
                    "openai" => "openai_api_key",
                    "xai" => "xai_api_key",
                    other => return format!("Unknown provider: {other}"),
                };
                Self::save_anvil_ui_config_key(config_key, serde_json::Value::String(key))
            }
            ConfigureAction::SetOllamaHost { url } => {
                Self::save_anvil_ui_config_key("ollama_host", serde_json::Value::String(url))
            }
            ConfigureAction::SetDefaultModel { model } => {
                Self::save_anvil_ui_config_key("default_model", serde_json::Value::String(model))
            }
            ConfigureAction::SetImageModel { model } => {
                Self::save_anvil_ui_config_key("image_model", serde_json::Value::String(model))
            }
            ConfigureAction::SetContextSize { size } => {
                Self::save_anvil_ui_config_key("context_size", serde_json::Value::Number(size.into()))
            }
            ConfigureAction::SetCompactThreshold { pct } => {
                Self::save_anvil_ui_config_key("compact_threshold", serde_json::Value::Number(u64::from(pct).into()))
            }
            ConfigureAction::SetQmdEnabled { enabled } => {
                Self::save_anvil_ui_config_key("qmd_enabled", serde_json::Value::Bool(enabled))
            }
            ConfigureAction::SetSearchKey { provider, key } => {
                let config_key = match provider.as_str() {
                    "Tavily" | "tavily" => "tavily_api_key",
                    "Brave" | "brave" => "brave_search_api_key",
                    "Exa" | "exa" => "exa_api_key",
                    "Perplexity" | "perplexity" => "perplexity_api_key",
                    "SearXNG" | "searxng" => "searxng_url",
                    other => return format!("Unknown search provider: {other}"),
                };
                Self::save_anvil_ui_config_key(config_key, serde_json::Value::String(key))
            }
            ConfigureAction::SetDefaultSearch { provider } => {
                Self::save_anvil_ui_config_key("default_search_provider", serde_json::Value::String(provider))
            }
            ConfigureAction::ToggleVim => {
                self.toggle_vim_mode()
            }
            ConfigureAction::ToggleChat => {
                match self.toggle_chat_mode() {
                    Ok(msg) => msg,
                    Err(e) => format!("chat toggle error: {e}"),
                }
            }
            ConfigureAction::SetPermissionMode { mode } => {
                match self.set_permissions(Some(mode)) {
                    Ok(_) => format!("Permissions set to: {}", self.permission_mode.as_str()),
                    Err(e) => format!("permissions error: {e}"),
                }
            }
            // ── Section 7: Language & Theme ───────────────────────────────
            ConfigureAction::SetLanguage { lang } => {
                Self::save_anvil_ui_config_key("language", serde_json::Value::String(lang.clone()))
            }
            ConfigureAction::SetTheme { theme } => {
                Self::save_anvil_ui_config_key("theme", serde_json::Value::String(theme.clone()))
            }
            // ── Section 8: Vault ─────────────────────────────────────────
            ConfigureAction::SetVaultSessionTtl { secs } => {
                Self::save_anvil_ui_config_key("vault_session_ttl", serde_json::Value::Number(secs.into()))
            }
            ConfigureAction::ToggleVaultAutoLock => {
                let cfg = Self::load_anvil_ui_config();
                let current = cfg.get("vault_auto_lock").and_then(serde_json::Value::as_bool).unwrap_or(false);
                Self::save_anvil_ui_config_key("vault_auto_lock", serde_json::Value::Bool(!current))
            }
            // ── Section 9: Notifications ─────────────────────────────────
            ConfigureAction::SetNotifyPlatform { platform } => {
                Self::save_anvil_ui_config_key("notify_platform", serde_json::Value::String(platform))
            }
            ConfigureAction::SetNotifyValue { key, value } => {
                Self::save_anvil_ui_config_key(&key, serde_json::Value::String(value))
            }
            // ── Section 10: Failover ─────────────────────────────────────
            ConfigureAction::SetFailoverCooldown { secs } => {
                Self::save_anvil_ui_config_key("failover_cooldown", serde_json::Value::Number(secs.into()))
            }
            ConfigureAction::SetFailoverBudget { budget } => {
                Self::save_anvil_ui_config_key("failover_budget", serde_json::Value::Number(budget.into()))
            }
            ConfigureAction::ToggleFailoverAutoRecovery => {
                let cfg = Self::load_anvil_ui_config();
                let current = cfg.get("failover_auto_recovery").and_then(serde_json::Value::as_bool).unwrap_or(true);
                Self::save_anvil_ui_config_key("failover_auto_recovery", serde_json::Value::Bool(!current))
            }
            // ── Section 11: SSH ──────────────────────────────────────────
            ConfigureAction::SetSshKeyPath { path } => {
                Self::save_anvil_ui_config_key("ssh_key_path", serde_json::Value::String(path))
            }
            ConfigureAction::SetSshBastionHost { host } => {
                Self::save_anvil_ui_config_key("ssh_bastion_host", serde_json::Value::String(host))
            }
            ConfigureAction::SetSshConfigPath { path } => {
                Self::save_anvil_ui_config_key("ssh_config_path", serde_json::Value::String(path))
            }
            // ── Section 12: Docker & K8s ─────────────────────────────────
            ConfigureAction::SetDockerComposeFile { path } => {
                Self::save_anvil_ui_config_key("docker_compose_file", serde_json::Value::String(path))
            }
            ConfigureAction::SetDockerRegistry { url } => {
                Self::save_anvil_ui_config_key("docker_registry", serde_json::Value::String(url))
            }
            ConfigureAction::SetK8sContext { ctx } => {
                Self::save_anvil_ui_config_key("k8s_context", serde_json::Value::String(ctx))
            }
            ConfigureAction::SetK8sNamespace { ns } => {
                Self::save_anvil_ui_config_key("k8s_namespace", serde_json::Value::String(ns))
            }
            // ── Section 13: Database ─────────────────────────────────────
            ConfigureAction::SetDbUrl { url } => {
                Self::save_anvil_ui_config_key("db_url", serde_json::Value::String(url))
            }
            ConfigureAction::SetDbSchemaTool { tool } => {
                Self::save_anvil_ui_config_key("db_schema_tool", serde_json::Value::String(tool))
            }
            // ── Section 14: Memory & Archive ─────────────────────────────
            ConfigureAction::ToggleAutoSaveMemory => {
                let cfg = Self::load_anvil_ui_config();
                let current = cfg.get("auto_save_memory").and_then(serde_json::Value::as_bool).unwrap_or(true);
                Self::save_anvil_ui_config_key("auto_save_memory", serde_json::Value::Bool(!current))
            }
            ConfigureAction::SetArchiveFrequency { n } => {
                Self::save_anvil_ui_config_key("archive_frequency", serde_json::Value::Number(n.into()))
            }
            ConfigureAction::SetArchiveRetention { days } => {
                Self::save_anvil_ui_config_key("archive_retention_days", serde_json::Value::Number(days.into()))
            }
            ConfigureAction::SetMemoryDir { path } => {
                Self::save_anvil_ui_config_key("memory_dir", serde_json::Value::String(path))
            }
            // ── Section 15: Plugins & Cron ───────────────────────────────
            ConfigureAction::SetPluginSearchPaths { paths } => {
                Self::save_anvil_ui_config_key("plugin_search_paths", serde_json::Value::String(paths))
            }
            ConfigureAction::ToggleAutoEnablePlugins => {
                let cfg = Self::load_anvil_ui_config();
                let current = cfg.get("auto_enable_plugins").and_then(serde_json::Value::as_bool).unwrap_or(false);
                Self::save_anvil_ui_config_key("auto_enable_plugins", serde_json::Value::Bool(!current))
            }
            ConfigureAction::ToggleCronEnabled => {
                let cfg = Self::load_anvil_ui_config();
                let current = cfg.get("cron_enabled").and_then(serde_json::Value::as_bool).unwrap_or(false);
                Self::save_anvil_ui_config_key("cron_enabled", serde_json::Value::Bool(!current))
            }
        }
    }

}
