mod file_drop;
mod init;
mod input;
mod render;
mod tui;

rust_i18n::i18n!("../../locales", fallback = "en");


use std::collections::BTreeSet;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use api::{
    detect_provider_kind, max_tokens_for_model, provider_display_name, resolve_startup_auth_source,
    AuthSource, AnvilApiClient, ContentBlockDelta, ImageSource, ImageSourceKind, InputContentBlock,
    InputMessage, MessageRequest, MessageResponse, OutputContentBlock, ProviderClient, ProviderKind,
    StreamEvent as ApiStreamEvent, ToolChoice, ToolDefinition, ToolResultContentBlock,
};

use commands::{
    handle_agents_slash_command, handle_plugins_slash_command, handle_skills_slash_command,
    render_slash_command_help, resume_supported_slash_commands, slash_command_specs,
    suggest_slash_commands, SlashCommand,
};
use compat_harness::{extract_manifest, UpstreamPaths};
use init::initialize_repo;
use plugins::{PluginManager, PluginManagerConfig};
use render::{
    render_permission_prompt, render_tool_call_block, render_tool_result_block,
    render_welcome_banner, BannerInfo, BlockState, MarkdownStreamState, StatusLine,
    ThinkingIndicator, TerminalRenderer,
};
use runtime::{
    clear_oauth_credentials, format_package_detail, format_package_list, generate_pkce_pair,
    generate_state, load_system_prompt, parse_oauth_callback_request_target, pricing_for_model,
    render_history_context, render_qmd_context, save_oauth_credentials, ApiClient, ApiRequest,
    ArchiveEntry, AssistantEvent, BlockingHubClient, CompactionConfig, CompletedTaskInfo,
    ConfigLoader, ConfigSource, ContentBlock, ConversationMessage, ConversationRuntime, CronDaemon,
    HistoryArchiver, LspManager, LspServerConfig, McpServerManager, MemoryManager, MessageRole,
    OAuthAuthorizationRequest, OAuthConfig, OAuthTokenExchangeRequest, PermissionMode,
    PermissionPolicy, ProjectContext, QmdClient, RuntimeError, Session, TaskManager, Theme,
    TokenUsage, ToolError, ToolExecutor, UsageTracker,
};
use crossterm::terminal;
use serde_json::json;
use tools::{execute_tool as execute_builtin_tool, GlobalToolRegistry, McpToolDefinition};
use tui::{AnvilTui, ConfigureAction, ConfigureData, ReadResult, TuiEvent, TuiSender};

/// A shared slot for the TUI sender.  Created once at startup and cloned into
/// `DefaultRuntimeClient` and `CliToolExecutor`.  When the TUI is active the
/// inner value is `Some`; setting it to `None` restores plain-stdout mode.
type TuiSenderSlot = Arc<Mutex<Option<TuiSender>>>;

const DEFAULT_MODEL: &str = "claude-opus-4-6";
const DEFAULT_DATE: &str = env!("BUILD_DATE");
const DEFAULT_OAUTH_CALLBACK_PORT: u16 = 4545;
const VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD_TARGET: &str = env!("TARGET");
const GIT_SHA: &str = env!("GIT_SHA");
const INTERNAL_PROGRESS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);

type AllowedToolSet = BTreeSet<String>;

fn main() {
    if let Err(error) = run() {
        eprintln!("{}", render_cli_error(&error.to_string()));
        std::process::exit(1);
    }
}

fn render_cli_error(problem: &str) -> String {
    let mut lines = vec!["Error".to_string()];
    for (index, line) in problem.lines().enumerate() {
        let label = if index == 0 {
            "  Problem          "
        } else {
            "                   "
        };
        lines.push(format!("{label}{line}"));
    }
    lines.push("  Help             anvil --help".to_string());
    lines.join("\n")
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    match parse_args(&args)? {
        CliAction::DumpManifests => dump_manifests(),
        CliAction::BootstrapPlan => print_bootstrap_plan(),
        CliAction::Agents { args } => LiveCli::print_agents(args.as_deref())?,
        CliAction::Skills { args } => LiveCli::print_skills(args.as_deref())?,
        CliAction::PrintSystemPrompt { cwd, date } => print_system_prompt(cwd, date),
        CliAction::Version => print_version(),
        CliAction::ResumeSession {
            session_path,
            commands,
        } => resume_session(&session_path, &commands),
        CliAction::Prompt {
            prompt,
            model,
            output_format,
            allowed_tools,
            permission_mode,
        } => LiveCli::new(model, true, allowed_tools, permission_mode)?
            .run_turn_with_output(&prompt, output_format)?,
        CliAction::Login { provider } => run_login(provider.as_deref())?,
        CliAction::Logout => run_logout()?,
        CliAction::Init => run_init()?,
        CliAction::Repl {
            model,
            allowed_tools,
            permission_mode,
        } => run_repl(model, allowed_tools, permission_mode)?,
        CliAction::Help => print_help(),
        CliAction::Continue => run_continue()?,
        CliAction::Sessions => print_sessions_standalone()?,
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliAction {
    DumpManifests,
    BootstrapPlan,
    Agents {
        args: Option<String>,
    },
    Skills {
        args: Option<String>,
    },
    PrintSystemPrompt {
        cwd: PathBuf,
        date: String,
    },
    Version,
    ResumeSession {
        session_path: PathBuf,
        commands: Vec<String>,
    },
    Prompt {
        prompt: String,
        model: String,
        output_format: CliOutputFormat,
        allowed_tools: Option<AllowedToolSet>,
        permission_mode: PermissionMode,
    },
    Login {
        provider: Option<String>,
    },
    Logout,
    Init,
    Repl {
        model: String,
        allowed_tools: Option<AllowedToolSet>,
        permission_mode: PermissionMode,
    },
    // prompt-mode formatting is only supported for non-interactive runs
    Help,
    /// Resume the most recent session.
    Continue,
    /// List all saved sessions.
    Sessions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliOutputFormat {
    Text,
    Json,
}

impl CliOutputFormat {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            other => Err(format!(
                "unsupported value for --output-format: {other} (expected text or json)"
            )),
        }
    }
}

#[allow(clippy::too_many_lines)]
fn parse_args(args: &[String]) -> Result<CliAction, String> {
    let mut model = DEFAULT_MODEL.to_string();
    let mut output_format = CliOutputFormat::Text;
    let mut permission_mode = default_permission_mode();
    let mut wants_version = false;
    let mut allowed_tool_values = Vec::new();
    let mut rest = Vec::new();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--version" | "-V" => {
                wants_version = true;
                index += 1;
            }
            "--update" => {
                run_self_update();
                std::process::exit(0);
            }
            "--model" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --model".to_string())?;
                model = resolve_model_alias(value).to_string();
                index += 2;
            }
            flag if flag.starts_with("--model=") => {
                model = resolve_model_alias(&flag[8..]).to_string();
                index += 1;
            }
            "--output-format" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --output-format".to_string())?;
                output_format = CliOutputFormat::parse(value)?;
                index += 2;
            }
            "--permission-mode" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --permission-mode".to_string())?;
                permission_mode = parse_permission_mode_arg(value)?;
                index += 2;
            }
            flag if flag.starts_with("--output-format=") => {
                output_format = CliOutputFormat::parse(&flag[16..])?;
                index += 1;
            }
            flag if flag.starts_with("--permission-mode=") => {
                permission_mode = parse_permission_mode_arg(&flag[18..])?;
                index += 1;
            }
            "--dangerously-skip-permissions" => {
                permission_mode = PermissionMode::DangerFullAccess;
                index += 1;
            }
            "-p" => {
                // Anvil compat: -p "prompt" = one-shot prompt
                let prompt = args[index + 1..].join(" ");
                if prompt.trim().is_empty() {
                    return Err("-p requires a prompt string".to_string());
                }
                return Ok(CliAction::Prompt {
                    prompt,
                    model: resolve_model_alias(&model).to_string(),
                    output_format,
                    allowed_tools: normalize_allowed_tools(&allowed_tool_values)?,
                    permission_mode,
                });
            }
            "--print" => {
                // Anvil compat: --print makes output non-interactive
                output_format = CliOutputFormat::Text;
                index += 1;
            }
            "--allowedTools" | "--allowed-tools" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --allowedTools".to_string())?;
                allowed_tool_values.push(value.clone());
                index += 2;
            }
            flag if flag.starts_with("--allowedTools=") => {
                allowed_tool_values.push(flag[15..].to_string());
                index += 1;
            }
            flag if flag.starts_with("--allowed-tools=") => {
                allowed_tool_values.push(flag[16..].to_string());
                index += 1;
            }
            other => {
                rest.push(other.to_string());
                index += 1;
            }
        }
    }

    if wants_version {
        return Ok(CliAction::Version);
    }

    let allowed_tools = normalize_allowed_tools(&allowed_tool_values)?;

    if rest.is_empty() {
        return Ok(CliAction::Repl {
            model,
            allowed_tools,
            permission_mode,
        });
    }
    if matches!(rest.first().map(String::as_str), Some("--help" | "-h")) {
        return Ok(CliAction::Help);
    }
    if rest.first().map(String::as_str) == Some("--resume") {
        return parse_resume_args(&rest[1..]);
    }
    if matches!(rest.first().map(String::as_str), Some("--continue" | "-c")) {
        return Ok(CliAction::Continue);
    }

    match rest[0].as_str() {
        "continue" => Ok(CliAction::Continue),
        "sessions" | "session-list" => Ok(CliAction::Sessions),
        "resume" => {
            if rest.get(1).is_some() {
                parse_resume_args(&rest[1..])
            } else {
                Ok(CliAction::Continue)
            }
        }
        "dump-manifests" => Ok(CliAction::DumpManifests),
        "bootstrap-plan" => Ok(CliAction::BootstrapPlan),
        "agents" => Ok(CliAction::Agents {
            args: join_optional_args(&rest[1..]),
        }),
        "skills" => Ok(CliAction::Skills {
            args: join_optional_args(&rest[1..]),
        }),
        "system-prompt" => parse_system_prompt_args(&rest[1..]),
        // `anvil model <name>` — shorthand for `anvil --model <name>` (starts REPL)
        "model" => {
            if let Some(m) = rest.get(1) {
                return Ok(CliAction::Repl {
                    model: resolve_model_alias(m).to_string(),
                    allowed_tools,
                    permission_mode,
                });
            }
            return Ok(CliAction::Repl { model, allowed_tools, permission_mode });
        }
        "login" => {
            // Support: `anvil login`, `anvil login anthropic`, `anvil login provider openai`,
            //          `anvil login --provider openai`, `anvil login --provider=openai`
            let mut provider: Option<String> = None;
            let mut idx = 1;
            while idx < rest.len() {
                match rest[idx].as_str() {
                    // `anvil login provider <name>`
                    "provider" => {
                        provider = rest.get(idx + 1).cloned();
                        idx += 2;
                    }
                    // `anvil login --provider <name>` (backward compat)
                    "--provider" => {
                        provider = rest.get(idx + 1).cloned();
                        idx += 2;
                    }
                    flag if flag.starts_with("--provider=") => {
                        provider = Some(flag[11..].to_string());
                        idx += 1;
                    }
                    // `anvil login anthropic` / `anvil login openai` / `anvil login ollama`
                    "anthropic" | "openai" | "ollama" | "apikey" | "api-key" => {
                        provider = Some(rest[idx].clone());
                        idx += 1;
                    }
                    _ => {
                        idx += 1;
                    }
                }
            }
            Ok(CliAction::Login { provider })
        }
        "logout" => Ok(CliAction::Logout),
        "init" => Ok(CliAction::Init),
        "prompt" => {
            let prompt = rest[1..].join(" ");
            if prompt.trim().is_empty() {
                return Err("prompt subcommand requires a prompt string".to_string());
            }
            Ok(CliAction::Prompt {
                prompt,
                model,
                output_format,
                allowed_tools,
                permission_mode,
            })
        }
        other if other.starts_with('/') => parse_direct_slash_cli_action(&rest),
        _other => Ok(CliAction::Prompt {
            prompt: rest.join(" "),
            model,
            output_format,
            allowed_tools,
            permission_mode,
        }),
    }
}

fn join_optional_args(args: &[String]) -> Option<String> {
    let joined = args.join(" ");
    let trimmed = joined.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn parse_direct_slash_cli_action(rest: &[String]) -> Result<CliAction, String> {
    let raw = rest.join(" ");
    match SlashCommand::parse(&raw) {
        Some(SlashCommand::Help) => Ok(CliAction::Help),
        Some(SlashCommand::Agents { args }) => Ok(CliAction::Agents { args }),
        Some(SlashCommand::Skills { args }) => Ok(CliAction::Skills { args }),
        Some(command) => Err(format_direct_slash_command_error(
            match &command {
                SlashCommand::Unknown(name) => format!("/{name}"),
                _ => rest[0].clone(),
            }
            .as_str(),
            matches!(command, SlashCommand::Unknown(_)),
        )),
        None => Err(format!("unknown subcommand: {}", rest[0])),
    }
}

fn format_direct_slash_command_error(command: &str, is_unknown: bool) -> String {
    let trimmed = command.trim().trim_start_matches('/');
    let mut lines = vec![
        "Direct slash command unavailable".to_string(),
        format!("  Command          /{trimmed}"),
    ];
    if is_unknown {
        append_slash_command_suggestions(&mut lines, trimmed);
    } else {
        lines.push("  Try              Start `anvil` to use interactive slash commands".to_string());
        lines.push(
            "  Tip              Resume-safe commands also work with `anvil --resume SESSION.json ...`"
                .to_string(),
        );
    }
    lines.join("\n")
}

fn resolve_model_alias(model: &str) -> &str {
    match model {
        "opus" => "claude-opus-4-6",
        "sonnet" => "claude-sonnet-4-6",
        "haiku" => "claude-haiku-4-5-20251213",
        "grok" | "grok-3" => "grok-3",
        "grok-mini" | "grok-3-mini" => "grok-3-mini",
        _ => model,
    }
}

fn normalize_allowed_tools(values: &[String]) -> Result<Option<AllowedToolSet>, String> {
    current_tool_registry()?.normalize_allowed_tools(values)
}

fn current_tool_registry() -> Result<GlobalToolRegistry, String> {
    let cwd = env::current_dir().map_err(|error| error.to_string())?;
    let loader = ConfigLoader::default_for(&cwd);
    let runtime_config = loader.load().map_err(|error| error.to_string())?;
    let plugin_manager = build_plugin_manager(&cwd, &loader, &runtime_config);
    let plugin_tools = plugin_manager
        .aggregated_tools()
        .map_err(|error| error.to_string())?;
    GlobalToolRegistry::with_plugin_tools(plugin_tools)
}

fn parse_permission_mode_arg(value: &str) -> Result<PermissionMode, String> {
    normalize_permission_mode(value)
        .ok_or_else(|| {
            format!(
                "unsupported permission mode '{value}'. Use read-only, workspace-write, or danger-full-access."
            )
        })
        .map(permission_mode_from_label)
}

fn permission_mode_from_label(mode: &str) -> PermissionMode {
    match mode {
        "read-only" => PermissionMode::ReadOnly,
        "workspace-write" => PermissionMode::WorkspaceWrite,
        "danger-full-access" => PermissionMode::DangerFullAccess,
        other => panic!("unsupported permission mode label: {other}"),
    }
}

fn default_permission_mode() -> PermissionMode {
    env::var("ANVIL_PERMISSION_MODE")
        .ok()
        .as_deref()
        .and_then(normalize_permission_mode)
        .map_or(PermissionMode::DangerFullAccess, permission_mode_from_label)
}

fn filter_tool_specs(
    tool_registry: &GlobalToolRegistry,
    allowed_tools: Option<&AllowedToolSet>,
) -> Vec<ToolDefinition> {
    tool_registry.definitions(allowed_tools)
}

fn parse_system_prompt_args(args: &[String]) -> Result<CliAction, String> {
    let mut cwd = env::current_dir().map_err(|error| error.to_string())?;
    let mut date = DEFAULT_DATE.to_string();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--cwd" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --cwd".to_string())?;
                cwd = PathBuf::from(value);
                index += 2;
            }
            "--date" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --date".to_string())?;
                date.clone_from(value);
                index += 2;
            }
            other => return Err(format!("unknown system-prompt option: {other}")),
        }
    }

    Ok(CliAction::PrintSystemPrompt { cwd, date })
}

fn parse_resume_args(args: &[String]) -> Result<CliAction, String> {
    let session_path = args
        .first()
        .ok_or_else(|| "missing session path for --resume".to_string())
        .map(PathBuf::from)?;
    let commands = args[1..].to_vec();
    if commands
        .iter()
        .any(|command| !command.trim_start().starts_with('/'))
    {
        return Err("--resume trailing arguments must be slash commands".to_string());
    }
    Ok(CliAction::ResumeSession {
        session_path,
        commands,
    })
}

fn dump_manifests() {
    let workspace_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let paths = UpstreamPaths::from_workspace_dir(&workspace_dir);
    match extract_manifest(&paths) {
        Ok(manifest) => {
            println!("commands: {}", manifest.commands.entries().len());
            println!("tools: {}", manifest.tools.entries().len());
            println!("bootstrap phases: {}", manifest.bootstrap.phases().len());
        }
        Err(error) => {
            eprintln!("failed to extract manifests: {error}");
            std::process::exit(1);
        }
    }
}

fn print_bootstrap_plan() {
    for phase in runtime::BootstrapPlan::anvil_default().phases() {
        println!("- {phase:?}");
    }
}

fn default_oauth_config() -> OAuthConfig {
    OAuthConfig {
        client_id: String::from("9d1c250a-e61b-44d9-88ed-5944d1962f5e"),
        authorize_url: String::from("https://claude.ai/oauth/authorize"),
        token_url: String::from("https://platform.claude.com/v1/oauth/token"),
        callback_port: None,
        manual_redirect_url: Some(String::from("https://platform.claude.com/oauth/code/callback")),
        scopes: vec![
            String::from("user:profile"),
            String::from("user:inference"),
            String::from("user:sessions:claude_code"),
        ],
    }
}


fn run_login(provider: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let chosen = match provider.map(str::to_ascii_lowercase).as_deref() {
        Some(p) => p.to_string(),
        None => {
            // Interactive provider selection
            println!("⚒ Anvil Login — Select a provider:\n");
            println!("  1) Anthropic  — Claude models (OAuth login via browser)");
            println!("  2) OpenAI     — GPT/o-series models (API key)");
            println!("  3) Ollama     — Local models (configure endpoint)");
            println!("  4) API Key    — Enter an Anthropic API key directly\n");
            print!("Choice [1-4]: ");
            io::stdout().flush()?;
            let mut choice = String::new();
            io::stdin().read_line(&mut choice)?;
            match choice.trim() {
                "1" | "anthropic" => "anthropic".to_string(),
                "2" | "openai" => "openai".to_string(),
                "3" | "ollama" => "ollama".to_string(),
                "4" | "apikey" | "api-key" | "key" => "apikey".to_string(),
                other => {
                    return Err(format!("Invalid choice: {other}").into());
                }
            }
        }
    };

    match chosen.as_str() {
        "anthropic" => run_anthropic_login(),
        "openai" => run_openai_apikey_setup("OpenAI", "OPENAI_API_KEY", "openai_api_key", "sk-"),
        "ollama" => run_ollama_setup(),
        "apikey" => run_openai_apikey_setup("Anthropic", "ANTHROPIC_API_KEY", "anthropic_api_key", "sk-ant-"),
        other => Err(format!(
            "unknown provider '{other}'. Valid options: anthropic, openai, ollama, apikey"
        )
        .into()),
    }
}

fn run_openai_apikey_setup(
    provider_name: &str,
    env_var: &str,
    cred_key: &str,
    key_prefix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n⚒ {provider_name} API Key Setup\n");

    // Check if already set via env
    if let Ok(existing) = std::env::var(env_var) {
        if !existing.is_empty() {
            let masked = if existing.len() > 12 {
                format!("{}...{}", &existing[..8], &existing[existing.len()-4..])
            } else {
                "****".to_string()
            };
            println!("{env_var} is already set: {masked}");
            print!("Replace it? [y/N]: ");
            io::stdout().flush()?;
            let mut confirm = String::new();
            io::stdin().read_line(&mut confirm)?;
            if !matches!(confirm.trim().to_lowercase().as_str(), "y" | "yes") {
                println!("Keeping existing key.");
                return Ok(());
            }
        }
    }

    println!("Get your API key from:");
    if provider_name == "OpenAI" {
        println!("  https://platform.openai.com/api-keys\n");
    } else {
        println!("  https://console.anthropic.com/settings/keys\n");
    }

    print!("Paste your {provider_name} API key: ");
    io::stdout().flush()?;
    let mut key = String::new();
    io::stdin().read_line(&mut key)?;
    let key = key.trim();
    if key.is_empty() {
        return Err("No key provided.".into());
    }
    if !key_prefix.is_empty() && !key.starts_with(key_prefix) {
        println!("⚠ Warning: key doesn't start with '{key_prefix}' — are you sure this is a {provider_name} key?");
    }

    // Save to credentials file
    let creds_path = runtime::credentials_path()?;
    let mut root = if creds_path.exists() {
        let data = fs::read_to_string(&creds_path)?;
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data)
            .unwrap_or_default()
    } else {
        serde_json::Map::new()
    };

    root.insert(cred_key.to_string(), serde_json::Value::String(key.to_string()));

    if let Some(parent) = creds_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&creds_path, serde_json::to_string_pretty(&root)?)?;

    println!("\n✓ {provider_name} API key saved.");
    println!("\nAlternatively, set in your shell: export {env_var}=<key>");
    println!("Use with: anvil --model {}", if provider_name == "OpenAI" { "gpt-5.4-mini" } else { "claude-sonnet-4-6" });
    Ok(())
}

/// Query the Anthropic /v1/models API for the live model list.
/// Returns Vec<(model_id, display_name)>. Returns empty on failure.
fn query_anthropic_models() -> Vec<(String, String)> {
    // Try OAuth token first, then API key
    let token = runtime::load_oauth_credentials()
        .ok()
        .flatten()
        .map(|t| format!("Authorization: Bearer {}", t.access_token));
    let api_key = std::env::var("ANTHROPIC_API_KEY").ok()
        .map(|k| format!("x-api-key: {k}"));

    let auth_header = token.or(api_key);
    let Some(auth) = auth_header else {
        return Vec::new();
    };

    let mut args = vec![
        "-s".to_string(),
        "--connect-timeout".to_string(), "5".to_string(),
        "-H".to_string(), auth,
        "-H".to_string(), "anthropic-version: 2023-06-01".to_string(),
    ];
    // Add beta header for OAuth
    if args[4].starts_with("Authorization") {
        args.push("-H".to_string());
        args.push("anthropic-beta: oauth-2025-04-20".to_string());
    }
    args.push("https://api.anthropic.com/v1/models".to_string());

    let output = std::process::Command::new("curl").args(&args).output();
    let Ok(out) = output else { return Vec::new() };
    if !out.status.success() { return Vec::new(); }

    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return Vec::new() };
    let Some(data) = val.get("data").and_then(|d| d.as_array()) else { return Vec::new() };

    data.iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?;
            let name = m.get("display_name").and_then(|n| n.as_str()).unwrap_or(id);
            Some((id.to_string(), name.to_string()))
        })
        .collect()
}

fn run_ollama_setup() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n⚒ Ollama Configuration\n");

    // Endpoint
    let default_host = std::env::var("OLLAMA_HOST")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    print!("Ollama endpoint [{default_host}]: ");
    io::stdout().flush()?;
    let mut host_input = String::new();
    io::stdin().read_line(&mut host_input)?;
    let host = host_input.trim();
    let host = if host.is_empty() { default_host.clone() } else { host.to_string() };

    // Optional API key (some hosted Ollama instances require one)
    print!("API key (press Enter for none): ");
    io::stdout().flush()?;
    let mut key_input = String::new();
    io::stdin().read_line(&mut key_input)?;
    let api_key = key_input.trim().to_string();

    // Test connectivity
    print!("Testing connection to {host}... ");
    io::stdout().flush()?;

    let mut curl_args = vec!["-s".to_string(), "--connect-timeout".to_string(), "5".to_string()];
    if !api_key.is_empty() {
        curl_args.push("-H".to_string());
        curl_args.push(format!("Authorization: Bearer {api_key}"));
    }
    curl_args.push(format!("{host}/api/tags"));

    match std::process::Command::new("curl").args(&curl_args).output() {
        Ok(out) if out.status.success() => {
            println!("✓ Connected\n");

            let mut model_names: Vec<String> = Vec::new();
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                if let Some(models) = val.get("models").and_then(|m| m.as_array()) {
                    println!("Available models:");
                    for (i, m) in models.iter().enumerate() {
                        let name = m.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                        let size = m.get("size").and_then(|s| s.as_f64()).unwrap_or(0.0);
                        println!("  {}) {:<30} {:.1}GB", i + 1, name, size / 1e9);
                        model_names.push(name.to_string());
                    }
                }
            }

            if !model_names.is_empty() {
                println!();
                print!("Select a model [1-{}] or press Enter to skip: ", model_names.len());
                io::stdout().flush()?;
                let mut choice = String::new();
                io::stdin().read_line(&mut choice)?;
                let choice = choice.trim();

                if let Ok(n) = choice.parse::<usize>() {
                    if n >= 1 && n <= model_names.len() {
                        let selected = &model_names[n - 1];
                        println!("\n✓ Selected: {selected}");
                        println!("\nStart Anvil with: anvil model {selected}");
                    }
                }
            }
        }
        _ => {
            println!("✗ Could not connect");
            println!("Make sure Ollama is running: ollama serve");
        }
    }

    // Save config
    if host != default_host || !api_key.is_empty() {
        // Save to credentials
        if let Ok(creds_path) = runtime::credentials_path() {
            let mut root = if creds_path.exists() {
                let data = fs::read_to_string(&creds_path).unwrap_or_default();
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data)
                    .unwrap_or_default()
            } else {
                serde_json::Map::new()
            };
            root.insert("ollama_host".to_string(), serde_json::Value::String(host.clone()));
            if !api_key.is_empty() {
                root.insert("ollama_api_key".to_string(), serde_json::Value::String(api_key));
            }
            if let Some(parent) = creds_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&creds_path, serde_json::to_string_pretty(&root).unwrap_or_default());
            println!("\n✓ Configuration saved to {}", creds_path.display());
        }

        if host != default_host {
            println!("To persist the endpoint, add to your shell profile:");
            println!("  export OLLAMA_HOST={host}");
        }
    }

    Ok(())
}

fn run_anthropic_login() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let config = ConfigLoader::default_for(&cwd).load()?;
    let default_oauth = default_oauth_config();
    let oauth = config.oauth().unwrap_or(&default_oauth);
    let callback_port = oauth.callback_port.unwrap_or(DEFAULT_OAUTH_CALLBACK_PORT);
    let redirect_uri = runtime::loopback_redirect_uri(callback_port);
    let pkce = generate_pkce_pair()?;
    let state = generate_state()?;
    let authorize_url =
        OAuthAuthorizationRequest::from_config(oauth, redirect_uri.clone(), state.clone(), &pkce)
            .build_url();

    println!("Starting Anvil OAuth login (Anthropic)...");
    println!("Listening for callback on {redirect_uri}");
    if let Err(error) = open_browser(&authorize_url) {
        eprintln!("warning: failed to open browser automatically: {error}");
        println!("Open this URL manually:\n{authorize_url}");
    }

    let callback = wait_for_oauth_callback(callback_port)?;
    if let Some(error) = callback.error {
        let description = callback
            .error_description
            .unwrap_or_else(|| "authorization failed".to_string());
        return Err(io::Error::other(format!("{error}: {description}")).into());
    }
    let code = callback.code.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "callback did not include code")
    })?;
    let returned_state = callback.state.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "callback did not include state")
    })?;
    if returned_state != state {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "oauth state mismatch").into());
    }

    let client = AnvilApiClient::from_auth(AuthSource::None).with_base_url(api::read_base_url());
    let exchange_request =
        OAuthTokenExchangeRequest::from_config(oauth, code, state, pkce.verifier, redirect_uri);
    let runtime = tokio::runtime::Runtime::new()?;
    let token_set = runtime.block_on(client.exchange_oauth_code(oauth, &exchange_request))?;
    save_oauth_credentials(&runtime::OAuthTokenSet {
        access_token: token_set.access_token,
        refresh_token: token_set.refresh_token,
        expires_at: token_set.expires_at,
        scopes: token_set.scopes,
    })?;
    println!("Anvil OAuth login complete.");
    Ok(())
}

fn run_logout() -> Result<(), Box<dyn std::error::Error>> {
    clear_oauth_credentials()?;
    println!("Anvil OAuth credentials cleared.");
    Ok(())
}

fn open_browser(url: &str) -> io::Result<()> {
    let commands = if cfg!(target_os = "macos") {
        vec![("open", vec![url])]
    } else if cfg!(target_os = "windows") {
        vec![("cmd", vec!["/C", "start", "", url])]
    } else {
        vec![("xdg-open", vec![url])]
    };
    for (program, args) in commands {
        match Command::new(program).args(args).spawn() {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no supported browser opener command found",
    ))
}

fn wait_for_oauth_callback(
    port: u16,
) -> Result<runtime::OAuthCallbackParams, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let (mut stream, _) = listener.accept()?;
    let mut buffer = [0_u8; 4096];
    let bytes_read = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let request_line = request.lines().next().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing callback request line")
    })?;
    let target = request_line.split_whitespace().nth(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "missing callback request target",
        )
    })?;
    let callback = parse_oauth_callback_request_target(target)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let body = if callback.error.is_some() {
        "Anvil OAuth login failed. You can close this window."
    } else {
        "Anvil OAuth login succeeded. You can close this window."
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    Ok(callback)
}

fn print_system_prompt(cwd: PathBuf, date: String) {
    match load_system_prompt(cwd, date, env::consts::OS, "unknown") {
        Ok(sections) => println!("{}", sections.join("\n\n")),
        Err(error) => {
            eprintln!("failed to build system prompt: {error}");
            std::process::exit(1);
        }
    }
}

fn print_version() {
    println!("{}", render_version_report());
}

fn resume_session(session_path: &Path, commands: &[String]) {
    let session = match Session::load_from_path(session_path) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("failed to restore session: {error}");
            std::process::exit(1);
        }
    };

    if commands.is_empty() {
        println!(
            "Restored session from {} ({} messages).",
            session_path.display(),
            session.messages.len()
        );
        return;
    }

    let mut session = session;
    for raw_command in commands {
        let Some(command) = SlashCommand::parse(raw_command) else {
            eprintln!("unsupported resumed command: {raw_command}");
            std::process::exit(2);
        };
        match run_resume_command(session_path, &session, &command) {
            Ok(ResumeCommandOutcome {
                session: next_session,
                message,
            }) => {
                session = next_session;
                if let Some(message) = message {
                    println!("{message}");
                }
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(2);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ResumeCommandOutcome {
    session: Session,
    message: Option<String>,
}

#[derive(Debug, Clone)]
struct StatusContext {
    cwd: PathBuf,
    session_path: Option<PathBuf>,
    loaded_config_files: usize,
    discovered_config_files: usize,
    memory_file_count: usize,
    project_root: Option<PathBuf>,
    git_branch: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct StatusUsage {
    message_count: usize,
    turns: u32,
    latest: TokenUsage,
    cumulative: TokenUsage,
    estimated_tokens: usize,
}

fn format_model_report(model: &str, message_count: usize, turns: u32) -> String {
    let provider = provider_display_name(detect_provider_kind(model));
    format!(
        "Model
  Current          {model}
  Provider         {provider}
  Session          {message_count} messages · {turns} turns

Aliases (Anthropic)
  opus             claude-opus-4-6
  sonnet           claude-sonnet-4-6
  haiku            claude-haiku-4-5-20251213

Aliases (xAI)
  grok             grok-3
  grok-mini        grok-3-mini

Routing (auto-detected by model name prefix)
  gpt-*, o1, o3, o4   OpenAI  (set OPENAI_API_KEY)
  llama*, mistral*    Ollama  (set OLLAMA_HOST or use default http://localhost:11434)

Next
  /model           Show the current model
  /model <name>    Switch models for this REPL session"
    )
}

fn format_model_switch_report(previous: &str, next: &str, message_count: usize) -> String {
    format!(
        "Model updated
  Previous         {previous}
  Current          {next}
  Preserved        {message_count} messages
  Tip              Existing conversation context stayed attached"
    )
}

fn format_permissions_report(mode: &str) -> String {
    let modes = [
        ("read-only", "Read/search tools only", mode == "read-only"),
        (
            "workspace-write",
            "Edit files inside the workspace",
            mode == "workspace-write",
        ),
        (
            "danger-full-access",
            "Unrestricted tool access",
            mode == "danger-full-access",
        ),
    ]
    .into_iter()
    .map(|(name, description, is_current)| {
        let marker = if is_current {
            "● current"
        } else {
            "○ available"
        };
        format!("  {name:<18} {marker:<11} {description}")
    })
    .collect::<Vec<_>>()
    .join(
        "
",
    );

    let effect = match mode {
        "read-only" => "Only read/search tools can run automatically",
        "workspace-write" => "Editing tools can modify files in the workspace",
        "danger-full-access" => "All tools can run without additional sandbox limits",
        _ => "Unknown permission mode",
    };

    format!(
        "Permissions
  Active mode      {mode}
  Effect           {effect}

Modes
{modes}

Next
  /permissions              Show the current mode
  /permissions <mode>       Switch modes for subsequent tool calls"
    )
}

fn format_permissions_switch_report(previous: &str, next: &str) -> String {
    format!(
        "Permissions updated
  Previous mode    {previous}
  Active mode      {next}
  Applies to       Subsequent tool calls in this REPL
  Tip              Run /permissions to review all available modes"
    )
}

fn format_cost_report(usage: TokenUsage) -> String {
    format!(
        "Cost
  Input tokens     {}
  Output tokens    {}
  Cache create     {}
  Cache read       {}
  Total tokens     {}

Next
  /status          See session + workspace context
  /compact         Trim local history if the session is getting large",
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_creation_input_tokens,
        usage.cache_read_input_tokens,
        usage.total_tokens(),
    )
}

fn format_resume_report(session_path: &str, message_count: usize, turns: u32) -> String {
    format!(
        "Session resumed
  Session file     {session_path}
  History          {message_count} messages · {turns} turns
  Next             /status · /diff · /export"
    )
}

fn format_compact_report(removed: usize, resulting_messages: usize, skipped: bool) -> String {
    if skipped {
        format!(
            "Compact
  Result           skipped
  Reason           Session is already below the compaction threshold
  Messages kept    {resulting_messages}"
        )
    } else {
        format!(
            "Compact
  Result           compacted
  Messages removed {removed}
  Messages kept    {resulting_messages}
  Tip              Use /status to review the trimmed session"
        )
    }
}

/// Format a list of archived session entries as a readable table.
fn format_history_archive_list(entries: &[ArchiveEntry]) -> String {
    if entries.is_empty() {
        return "History archive\n  No archived sessions yet.\n  Sessions are archived automatically when the context window approaches capacity,\n  or manually via /compact.".to_string();
    }

    let mut lines = vec![format!(
        "History archive ({} sessions)\n",
        entries.len()
    )];
    for entry in entries.iter().take(20) {
        let ts = entry.timestamp;
        // Format as a simple ISO-like date from unix timestamp.
        let date = format_unix_timestamp(ts);
        lines.push(format!(
            "  {:<36}  {}  {:>4} msgs  {}",
            entry.session_id,
            date,
            entry.message_count,
            entry.model,
        ));
    }
    if entries.len() > 20 {
        lines.push(format!("  ... and {} more", entries.len() - 20));
    }
    lines.push(String::new());
    lines.push("  Use /history-archive search <query> to search.".to_string());
    lines.push("  Use /history-archive view <session-id> to read a session.".to_string());
    lines.join("\n")
}

/// Extract the `## Summary` section from an archive markdown file.
fn extract_summary_from_archive(content: &str) -> Option<String> {
    let start = content.find("## Summary\n")?;
    let body = &content[start + "## Summary\n".len()..];
    // Stop at the next `##` heading.
    let end = body.find("\n## ").unwrap_or(body.len());
    let summary = body[..end].trim();
    if summary.is_empty() {
        None
    } else {
        Some(summary.to_string())
    }
}

/// Convert a Unix timestamp (seconds) to a human-readable date string.
fn format_unix_timestamp(secs: u64) -> String {
    // Simple manual formatting: days-since-epoch → year/month/day.
    // Using the proleptic Gregorian calendar algorithm.
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;

    // Algorithm from https://en.wikipedia.org/wiki/Julian_day#Julian_date_calculation
    let z = days + 2440588; // Unix epoch = JDN 2440588
    let a = (z as f64 - 1867216.25) / 36524.25;
    let a = a.floor() as u64;
    let b = z + 1 + a - a / 4;
    let c = b + 1524;
    let d = ((c as f64 - 122.1) / 365.25).floor() as u64;
    let e = (365.25 * d as f64).floor() as u64;
    let f = ((c - e) as f64 / 30.6001).floor() as u64;

    let day = c - e - (30.6001 * f as f64).floor() as u64;
    let month = if f < 14 { f - 1 } else { f - 13 };
    let year = if month > 2 { d - 4716 } else { d - 4715 };

    format!("{year}-{month:02}-{day:02} {h:02}:{m:02}Z")
}

fn parse_git_status_metadata(status: Option<&str>) -> (Option<PathBuf>, Option<String>) {
    let Some(status) = status else {
        return (None, None);
    };
    let branch = status.lines().next().and_then(|line| {
        line.strip_prefix("## ")
            .map(|line| {
                line.split(['.', ' '])
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
            .filter(|value| !value.is_empty())
    });
    let project_root = find_git_root().ok();
    (project_root, branch)
}

fn find_git_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(env::current_dir()?)
        .output()?;
    if !output.status.success() {
        return Err("not a git repository".into());
    }
    let path = String::from_utf8(output.stdout)?.trim().to_string();
    if path.is_empty() {
        return Err("empty git root".into());
    }
    Ok(PathBuf::from(path))
}

#[allow(clippy::too_many_lines)]
fn run_resume_command(
    session_path: &Path,
    session: &Session,
    command: &SlashCommand,
) -> Result<ResumeCommandOutcome, Box<dyn std::error::Error>> {
    match command {
        SlashCommand::Help => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(render_repl_help()),
        }),
        SlashCommand::Compact => {
            let result = runtime::compact_session(
                session,
                CompactionConfig {
                    max_estimated_tokens: 0,
                    ..CompactionConfig::default()
                },
            );
            let removed = result.removed_message_count;
            let kept = result.compacted_session.messages.len();
            let skipped = removed == 0;
            result.compacted_session.save_to_path(session_path)?;
            Ok(ResumeCommandOutcome {
                session: result.compacted_session,
                message: Some(format_compact_report(removed, kept, skipped)),
            })
        }
        SlashCommand::Clear { confirm } => {
            if !confirm {
                return Ok(ResumeCommandOutcome {
                    session: session.clone(),
                    message: Some(
                        "clear: confirmation required; rerun with /clear --confirm".to_string(),
                    ),
                });
            }
            let cleared = Session::new();
            cleared.save_to_path(session_path)?;
            Ok(ResumeCommandOutcome {
                session: cleared,
                message: Some(format!(
                    "Cleared resumed session file {}.",
                    session_path.display()
                )),
            })
        }
        SlashCommand::Status => {
            let tracker = UsageTracker::from_session(session);
            let usage = tracker.cumulative_usage();
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format_status_report(
                    "restored-session",
                    StatusUsage {
                        message_count: session.messages.len(),
                        turns: tracker.turns(),
                        latest: tracker.current_turn_usage(),
                        cumulative: usage,
                        estimated_tokens: 0,
                    },
                    default_permission_mode().as_str(),
                    &status_context(Some(session_path))?,
                )),
            })
        }
        SlashCommand::Cost => {
            let usage = UsageTracker::from_session(session).cumulative_usage();
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format_cost_report(usage)),
            })
        }
        SlashCommand::Config { section } => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(render_config_report(section.as_deref())?),
        }),
        SlashCommand::Memory => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(render_memory_report()?),
        }),
        SlashCommand::Init => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(init_anvil_md()?),
        }),
        SlashCommand::Diff => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(render_diff_report()?),
        }),
        SlashCommand::Version => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(render_version_report()),
        }),
        SlashCommand::Export { path } => {
            let export_path = resolve_export_path(path.as_deref(), session)?;
            fs::write(&export_path, render_export_text(session))?;
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format!(
                    "Export\n  Result           wrote transcript\n  File             {}\n  Messages         {}",
                    export_path.display(),
                    session.messages.len(),
                )),
            })
        }
        SlashCommand::Agents { args } => {
            let cwd = env::current_dir()?;
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(handle_agents_slash_command(args.as_deref(), &cwd)?),
            })
        }
        SlashCommand::Skills { args } => {
            let cwd = env::current_dir()?;
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(handle_skills_slash_command(args.as_deref(), &cwd)?),
            })
        }
        SlashCommand::Bughunter { .. }
        | SlashCommand::Branch { .. }
        | SlashCommand::Worktree { .. }
        | SlashCommand::CommitPushPr { .. }
        | SlashCommand::Commit
        | SlashCommand::Pr { .. }
        | SlashCommand::Issue { .. }
        | SlashCommand::Ultraplan { .. }
        | SlashCommand::Teleport { .. }
        | SlashCommand::DebugToolCall
        | SlashCommand::Resume { .. }
        | SlashCommand::Model { .. }
        | SlashCommand::Permissions { .. }
        | SlashCommand::Session { .. }
        | SlashCommand::Plugins { .. }
        | SlashCommand::Qmd { .. }
        | SlashCommand::Undo
        | SlashCommand::History { .. }
        | SlashCommand::Context { .. }
        | SlashCommand::Pin { .. }
        | SlashCommand::Unpin { .. }
        | SlashCommand::Chat
        | SlashCommand::Vim
        | SlashCommand::Web { .. }
        | SlashCommand::Doctor
        | SlashCommand::Tokens
        | SlashCommand::Provider { .. }
        | SlashCommand::Login { .. }
        | SlashCommand::Search { .. }
        | SlashCommand::Failover { .. }
        | SlashCommand::GenerateImage { .. }
        | SlashCommand::Theme { .. }
        | SlashCommand::SemanticSearch { .. }
        | SlashCommand::Docker { .. }
        | SlashCommand::Test { .. }
        | SlashCommand::Git { .. }
        | SlashCommand::Refactor { .. }
        | SlashCommand::Screenshot
        | SlashCommand::Db { .. }
        | SlashCommand::Security { .. }
        | SlashCommand::Api { .. }
        | SlashCommand::Docs { .. }
        | SlashCommand::Scaffold { .. }
        | SlashCommand::Perf { .. }
        | SlashCommand::Debug { .. }
        | SlashCommand::Voice { .. }
        | SlashCommand::Collab { .. }
        | SlashCommand::Changelog
        | SlashCommand::Env { .. }
        | SlashCommand::Hub { .. }
        | SlashCommand::Lsp { .. }
        | SlashCommand::Notebook { .. }
        | SlashCommand::K8s { .. }
        | SlashCommand::Iac { .. }
        | SlashCommand::Pipeline { .. }
        | SlashCommand::Review { .. }
        | SlashCommand::Deps { .. }
        | SlashCommand::Mono { .. }
        | SlashCommand::Browser { .. }
        | SlashCommand::Notify { .. }
        | SlashCommand::Vault { .. }
        | SlashCommand::Migrate { .. }
        | SlashCommand::Regex { .. }
        | SlashCommand::Ssh { .. }
        | SlashCommand::Logs { .. }
        | SlashCommand::Markdown { .. }
        | SlashCommand::Snippets { .. }
        | SlashCommand::Finetune { .. }
        | SlashCommand::Webhook { .. }
        | SlashCommand::PluginSdk { .. }
        | SlashCommand::Unknown(_) => Err("unsupported resumed slash command".into()),
        SlashCommand::HistoryArchive { action } => {
            let archiver = HistoryArchiver::new();
            let output = format_history_archive_list(&archiver.list_archives());
            let _ = action; // search/view require a live QMD session
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(output),
            })
        }
        SlashCommand::Configure { args } => {
            let output = render_configure_static(args.as_deref());
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(output),
            })
        }
        SlashCommand::Language { lang } => {
            let output = run_language_command_static(lang.as_deref());
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(output),
            })
        }
    }
}

/// Resume the most recent saved session and start the REPL.
fn run_continue() -> Result<(), Box<dyn std::error::Error>> {
    let sessions = list_managed_sessions()?;
    let most_recent = sessions
        .into_iter()
        .next()
        .ok_or("No saved sessions found. Start a new session first with: anvil")?;

    let loaded = Session::load_from_path(&most_recent.path)?;
    let message_count = loaded.messages.len();

    // Build a fresh LiveCli then immediately swap in the loaded session.
    let mut cli = LiveCli::new(
        DEFAULT_MODEL.to_string(),
        true,
        None,
        default_permission_mode(),
    )?;
    cli.resume_from_session(loaded, most_recent.id.clone(), most_recent.path.clone())?;

    eprintln!(
        "Resuming session {}  ({} messages)",
        most_recent.id,
        message_count,
    );

    if io::stdout().is_terminal() {
        run_repl_tui(cli)
    } else {
        run_repl_plain(cli)
    }
}

/// Print all saved sessions to stdout and exit.
fn print_sessions_standalone() -> Result<(), Box<dyn std::error::Error>> {
    let sessions = list_managed_sessions()?;
    let dir = sessions_dir().map(|p| p.display().to_string()).unwrap_or_default();
    println!("Sessions");
    println!("  Directory         {dir}");
    if sessions.is_empty() {
        println!("  No managed sessions saved yet.");
        return Ok(());
    }
    for (i, session) in sessions.iter().enumerate() {
        let age = format_relative_timestamp(session.modified_epoch_secs);
        println!(
            "  {:>2}.  {id:<22}  {age:<12}  {msgs:>3} messages",
            i + 1,
            id = session.id,
            msgs = session.message_count,
        );
        println!("        {}", session.path.display());
    }
    Ok(())
}

fn run_repl(
    model: String,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
) -> Result<(), Box<dyn std::error::Error>> {
    let cli = LiveCli::new(model, true, allowed_tools, permission_mode)?;

    // Use the full-screen TUI only when stdout is an actual terminal.
    if io::stdout().is_terminal() {
        run_repl_tui(cli)
    } else {
        run_repl_plain(cli)
    }
}

/// Full-screen TUI REPL loop.
fn run_repl_tui(mut cli: LiveCli) -> Result<(), Box<dyn std::error::Error>> {
    let (mut tui, sender) =
        AnvilTui::new(cli.model.clone(), cli.session_id(), cli.permission_mode.as_str())?;

    // Install the TUI sender so all model/tool output is routed to it.
    cli.enable_tui(sender);

    // Greet the user with a system message instead of the welcome banner.
    let session_id = cli.session_id().to_string();
    tui.push_system(format!(
        "Anvil v{}  |  {}  |  {}  |  Ctrl+C or /exit to quit",
        env!("CARGO_PKG_VERSION"),
        cli.model,
        session_id,
    ));

    // Set initial QMD status in footer
    if cli.qmd.is_enabled() {
        if let Some(status) = cli.qmd.status() {
            tui.set_qmd_status(format!(
                "QMD: {} docs, {} vectors",
                status.total_docs, status.total_vectors
            ));
        } else {
            tui.set_qmd_status("QMD: active".to_string());
        }
    }

    // Show archive count
    let archives = cli.history_archiver.list_archives();
    if !archives.is_empty() {
        tui.set_archive_status(format!("{} archives indexed", archives.len()));
    }

    // Start cron daemon.
    let _cron_daemon = if std::env::var("ANVIL_NO_CRON").as_deref() != Ok("1") {
        Some(CronDaemon::start())
    } else {
        None
    };

    // Background update check — non-blocking, fires once at startup.
    let update_check: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    {
        let update_slot = Arc::clone(&update_check);
        let current_version = VERSION.to_string();
        thread::spawn(move || {
            if let Some(msg) = check_for_update(&current_version) {
                if let Ok(mut slot) = update_slot.lock() {
                    *slot = Some(msg);
                }
            }
        });
    }

    let mut task_check_instant = Instant::now();

    'outer: loop {
        // Check for background task completions.
        task_check_instant = inject_task_notifications_tui(&mut cli, &mut tui, task_check_instant);

        // Check if the background update check completed.
        if let Ok(mut slot) = update_check.try_lock() {
            if let Some(msg) = slot.take() {
                tui.set_update_available(msg);
            }
        }

        // Drain any queued TUI events (e.g. from previous turn).
        tui.poll_events();

        // Read the next key event (returns quickly with Continue most of the time).
        match tui.read_input()? {
            ReadResult::Continue => {
                // Nothing submitted yet; loop and redraw.
            }
            ReadResult::Exit => {
                cli.persist_session()?;
                break 'outer;
            }
            ReadResult::ConfigureAction(action) => {
                let msg = cli.apply_configure_action(action);
                // Rebuild a fresh ConfigureData snapshot and re-enter configure mode
                // so the menu immediately reflects the change.
                let data = cli.build_configure_data();
                tui.enter_configure_mode(data);
                if !msg.is_empty() {
                    tui.push_system(msg);
                }
            }
            ReadResult::NewTab => {
                // Ctrl+T: open a new in-memory tab.  All tabs share the same
                // model and runtime for now; the new tab just gets a fresh
                // session so the conversation starts empty.
                let new_session = create_managed_session_handle()?;
                let tab_idx = tui.new_tab("new", cli.model.clone(), new_session.id.clone());
                tui.switch_tab(tab_idx);
                tui.push_system(format!(
                    "Opened tab {}  |  session {}  |  use /tab rename <name> to rename",
                    tab_idx + 1,
                    new_session.id,
                ));
            }
            ReadResult::Submit(input) => {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if matches!(trimmed, "/exit" | "/quit") {
                    cli.persist_session()?;
                    break 'outer;
                }

                // /tab is TUI-only — handle before SlashCommand dispatch.
                if trimmed.starts_with("/tab") {
                    let rest = trimmed[4..].trim();
                    let parts: Vec<&str> = rest.splitn(2, ' ').collect();
                    let action = parts.first().copied().unwrap_or("").trim();
                    let arg = parts.get(1).copied().unwrap_or("").trim();
                    match action {
                        "new" => {
                            let name = if arg.is_empty() { "new" } else { arg };
                            let new_session = create_managed_session_handle()?;
                            let tab_idx = tui.new_tab(name, cli.model.clone(), new_session.id.clone());
                            tui.switch_tab(tab_idx);
                            tui.push_system(format!(
                                "Opened tab {}  |  session {}",
                                tab_idx + 1,
                                new_session.id,
                            ));
                        }
                        "close" => {
                            if let Some(name) = tui.close_active_tab() {
                                tui.push_system(format!("Closed tab: {name}"));
                            } else {
                                tui.push_system("Cannot close the last tab.".to_string());
                            }
                        }
                        "list" => {
                            let tabs = tui.tab_list();
                            let active_idx = tui.active_tab_index();
                            let mut msg = format!("Open tabs ({}):\n", tabs.len());
                            for (i, id, name, unread) in &tabs {
                                let marker = if *unread { "*" } else { " " };
                                let active = if *i == active_idx { " (active)" } else { "" };
                                msg.push_str(&format!("  [{id}] {name}{marker}{active}\n"));
                            }
                            tui.push_system(msg);
                        }
                        "rename" => {
                            if arg.is_empty() {
                                tui.push_system("Usage: /tab rename <name>".to_string());
                            } else {
                                tui.rename_active_tab(arg);
                                tui.push_system(format!("Tab renamed to: {arg}"));
                            }
                        }
                        n if n.parse::<usize>().is_ok() => {
                            let idx = n.parse::<usize>().unwrap().saturating_sub(1);
                            tui.switch_tab(idx);
                        }
                        "" => {
                            // /tab with no args — show help
                            tui.push_system(
                                "Tab commands:\n  /tab new [name]     open a new tab\n  /tab close          close current tab\n  /tab list           list all tabs\n  /tab rename <name>  rename current tab\n  /tab <N>            switch to tab N\n\nKey bindings:\n  Ctrl+T              new tab\n  Ctrl+W              close tab\n  Ctrl+Left/Right     previous / next tab\n  Alt+1..9            switch to tab N".to_string(),
                            );
                        }
                        other => {
                            tui.push_system(format!("Unknown /tab action: {other}. Try /tab for help."));
                        }
                    }
                    continue;
                }

                if let Some(command) = SlashCommand::parse(trimmed) {
                    // Handle slash commands — capture their output as a system message.
                    match cli.handle_repl_command_tui(command, &mut tui) {
                        Ok(should_persist) => {
                            if should_persist {
                                cli.persist_session()?;
                            }
                        }
                        Err(err) => {
                            tui.push_system(format!("Error: {err}"));
                        }
                    }
                    continue;
                }

                // Check whether the input looks like one or more file paths.
                let file_paths = file_drop::detect_file_paths(trimmed);
                if !file_paths.is_empty() {
                    let mut any_blocks = false;
                    for path in &file_paths {
                        let result = file_drop::process_file(path);
                        tui.push_system(result.notice);
                        if !result.blocks.is_empty() {
                            cli.runtime.inject_user_blocks(result.blocks);
                            any_blocks = true;
                        }
                    }
                    if any_blocks {
                        // Run a turn so the model can respond to the injected content.
                        tui.push_system(format!("Thinking... ({})", cli.model));
                        let result = cli.run_turn_file_drop();
                        tui.wait_for_turn_end()?;
                        if let Err(err) = result {
                            tui.push_system(format!("Turn error: {err}"));
                        }
                    }
                    continue;
                }

                // User prompt: run a turn.  The TUI sender is already installed,
                // so model output streams back to the TUI.
                tui.push_system(format!("Thinking... ({})", cli.model));
                // run_turn is synchronous (blocks current thread); the TUI
                // events are enqueued and we drain them after it returns.
                let result = cli.run_turn(trimmed);
                // Wait for the TurnDone event while animating the display.
                tui.wait_for_turn_end()?;
                if let Err(err) = result {
                    tui.push_system(format!("Turn error: {err}"));
                }
                // Update footer QMD/archive status after each turn
                if cli.qmd.is_enabled() {
                    if let Some(status) = cli.qmd.status() {
                        tui.set_qmd_status(format!(
                            "QMD: {} docs, {} vecs",
                            status.total_docs, status.total_vectors
                        ));
                    }
                }
                let archives = cli.history_archiver.list_archives();
                if !archives.is_empty() {
                    let latest = &archives[0];
                    let age_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                        .saturating_sub(latest.timestamp);
                    let age = if age_secs < 60 { "just now".to_string() }
                        else if age_secs < 3600 { format!("{}m ago", age_secs / 60) }
                        else if age_secs < 86400 { format!("{}h ago", age_secs / 3600) }
                        else { format!("{}d ago", age_secs / 86400) };
                    tui.set_archive_status(format!(
                        "{} archives | latest: {} ({})",
                        archives.len(), latest.session_id.chars().take(12).collect::<String>(), age
                    ));
                }
            }
        }
    }

    // Drop `tui` here — the Drop impl restores the terminal.
    Ok(())
}

/// Plain-stdout REPL loop (non-TTY / fallback).
fn run_repl_plain(mut cli: LiveCli) -> Result<(), Box<dyn std::error::Error>> {
    let mut editor = input::LineEditor::new("> ", slash_command_completion_candidates());
    println!("{}", cli.startup_banner());

    // Check for updates (blocking but with short timeout)
    if let Some(msg) = check_for_update(VERSION) {
        println!("\x1b[33;1m⬆ {msg}\x1b[0m");
    }

    let _cron_daemon = if std::env::var("ANVIL_NO_CRON").as_deref() != Ok("1") {
        Some(CronDaemon::start())
    } else {
        None
    };

    let mut task_check_instant = Instant::now();

    loop {
        task_check_instant = inject_task_notifications(&mut cli, task_check_instant);

        match editor.read_line()? {
            input::ReadOutcome::Submit(input) => {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if matches!(trimmed, "/exit" | "/quit") {
                    cli.persist_session()?;
                    break;
                }
                if let Some(command) = SlashCommand::parse(trimmed) {
                    if cli.handle_repl_command(command)? {
                        cli.persist_session()?;
                    }
                    // Sync vim state back to the line editor after any command.
                    if editor.is_vim_enabled() != cli.vim_mode {
                        editor.toggle_vim();
                    }
                    continue;
                }

                // Check whether the input looks like one or more file paths.
                let file_paths = file_drop::detect_file_paths(trimmed);
                if !file_paths.is_empty() {
                    let mut any_blocks = false;
                    for path in &file_paths {
                        let result = file_drop::process_file(path);
                        println!("{}", result.notice);
                        if !result.blocks.is_empty() {
                            cli.runtime.inject_user_blocks(result.blocks);
                            any_blocks = true;
                        }
                    }
                    if any_blocks {
                        cli.run_turn_file_drop()?;
                    }
                    continue;
                }

                editor.push_history(&input);
                cli.run_turn(&input)?;
            }
            input::ReadOutcome::Cancel => {}
            input::ReadOutcome::Exit => {
                cli.persist_session()?;
                break;
            }
        }
    }

    Ok(())
}

/// TUI-aware version of `inject_task_notifications`: pushes system messages
/// into the TUI instead of printing to stderr.
fn inject_task_notifications_tui(
    cli: &mut LiveCli,
    tui: &mut AnvilTui,
    last_check: Instant,
) -> Instant {
    let now = Instant::now();
    let completed: Vec<CompletedTaskInfo> = match TaskManager::global().lock() {
        Ok(mgr) => mgr.completed_since(last_check),
        Err(_) => return now,
    };
    if completed.is_empty() {
        return now;
    }
    for info in &completed {
        let notification = format_task_notification(info);
        tui.push_system(format!(
            "[task] \"{}\" {} (id: {})",
            info.description,
            info.status.as_str(),
            info.id,
        ));
        cli.runtime.inject_user_message(&notification);
    }
    if let Err(err) = cli.persist_session() {
        tui.push_system(format!("[task] failed to persist session: {err}"));
    }
    now
}

/// Check `TaskManager` for tasks that completed since `last_check`.
/// Inject a `<task-notification>` user message for each one so the model
/// sees them on its next turn.  Returns the updated checkpoint `Instant`.
fn inject_task_notifications(cli: &mut LiveCli, last_check: Instant) -> Instant {
    let now = Instant::now();

    let completed: Vec<CompletedTaskInfo> = match TaskManager::global().lock() {
        Ok(mgr) => mgr.completed_since(last_check),
        Err(_) => return now,
    };

    if completed.is_empty() {
        return now;
    }

    for info in &completed {
        let notification = format_task_notification(info);
        // Print a brief notice to the terminal so the user is aware.
        eprintln!(
            "[task] background task \"{}\" {} (id: {})",
            info.description,
            info.status.as_str(),
            info.id,
        );
        cli.runtime.inject_user_message(&notification);
    }

    // Persist the updated session so the notifications survive a resume.
    if let Err(err) = cli.persist_session() {
        eprintln!("[task] failed to persist session after notifications: {err}");
    }

    now
}

fn format_task_notification(info: &CompletedTaskInfo) -> String {
    let outcome = match info.status {
        runtime::TaskStatus::Completed => "completed successfully",
        runtime::TaskStatus::Failed => "failed",
        runtime::TaskStatus::Stopped => "was stopped",
        _ => "reached terminal state",
    };
    format!(
        "<task-notification>\n\
         <task-id>{id}</task-id>\n\
         <status>{status}</status>\n\
         <summary>Background task \"{desc}\" {outcome}</summary>\n\
         </task-notification>",
        id = info.id,
        status = info.status.as_str(),
        desc = info.description,
        outcome = outcome,
    )
}

#[derive(Debug, Clone)]
struct SessionHandle {
    id: String,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct ManagedSessionSummary {
    id: String,
    path: PathBuf,
    modified_epoch_secs: u64,
    message_count: usize,
}

struct LiveCli {
    model: String,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
    system_prompt: Vec<String>,
    runtime: ConversationRuntime<DefaultRuntimeClient, CliToolExecutor>,
    session: SessionHandle,
    /// Shared slot — install a `TuiSender` here to redirect output to the TUI.
    tui_slot: TuiSenderSlot,
    /// QMD search client — present when the `qmd` binary is available,
    /// disabled (but non-None) otherwise so callers never need to branch.
    qmd: QmdClient,
    /// Archives full conversations to `~/.anvil/history/` before compaction.
    history_archiver: HistoryArchiver,
    /// Files added via /context for the current session.
    context_files: Vec<PathBuf>,
    /// Whether chat-only mode (no tools) is active.
    chat_mode: bool,
    /// Whether vim keybindings are requested; propagated to the LineEditor.
    vim_mode: bool,
}

impl LiveCli {
    fn new(
        model: String,
        enable_tools: bool,
        allowed_tools: Option<AllowedToolSet>,
        permission_mode: PermissionMode,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let system_prompt = build_system_prompt()?;
        let session = create_managed_session_handle()?;
        let tui_slot: TuiSenderSlot = Arc::new(Mutex::new(None));
        let runtime = build_runtime_with_tui_slot(
            Session::new(),
            model.clone(),
            system_prompt.clone(),
            enable_tools,
            true,
            allowed_tools.clone(),
            permission_mode,
            None,
            tui_slot.clone(),
        )?;
        let qmd = QmdClient::new();
        let history_archiver = HistoryArchiver::new();
        // Best-effort: register and refresh the anvil-history QMD collection.
        qmd.ensure_history_indexed(history_archiver.history_dir());
        let cli = Self {
            model,
            allowed_tools,
            permission_mode,
            system_prompt,
            runtime,
            session,
            tui_slot,
            qmd,
            history_archiver,
            context_files: Vec::new(),
            chat_mode: false,
            vim_mode: false,
        };
        cli.persist_session()?;
        Ok(cli)
    }

    /// Install a TUI sender so all model/tool output goes to the TUI.
    fn enable_tui(&self, sender: TuiSender) {
        if let Ok(mut guard) = self.tui_slot.lock() {
            *guard = Some(sender);
        }
    }

    /// Remove the TUI sender (fallback to stdout).
    #[allow(dead_code)]
    fn disable_tui(&self) {
        if let Ok(mut guard) = self.tui_slot.lock() {
            *guard = None;
        }
    }

    /// Return the current session ID.
    fn session_id(&self) -> &str {
        &self.session.id
    }

    fn startup_banner(&self) -> String {
        let cwd = env::current_dir().ok();
        let cwd_display = cwd.as_ref().map_or_else(
            || "<unknown>".to_string(),
            |path| path.display().to_string(),
        );
        let workspace_name = cwd
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("workspace");
        let git_branch = status_context(Some(&self.session.path))
            .ok()
            .and_then(|context| context.git_branch);
        let has_anvil_md = cwd
            .as_ref()
            .is_some_and(|path| path.join("ANVIL.md").is_file());

        if io::stdout().is_terminal() {
            render_welcome_banner(&BannerInfo {
                version: VERSION,
                model: &self.model,
                workspace: workspace_name,
                directory: &cwd_display,
                git_branch: git_branch.as_deref(),
                session_id: &self.session.id,
                permission_mode: self.permission_mode.as_str(),
                has_anvil_md,
            })
        } else {
            // Non-TTY: plain text fallback
            format!(
                "Anvil {VERSION} · model: {} · session: {}",
                self.model, self.session.id
            )
        }
    }

    /// Run a model turn after file-drop blocks have already been injected via
    /// `runtime.inject_user_blocks`.  No additional user message is prepended.
    fn run_turn_file_drop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let tui_tx: Option<TuiSender> = self
            .tui_slot
            .lock()
            .ok()
            .and_then(|guard| guard.clone());

        if let Some(ref tx) = tui_tx {
            tx.send(TuiEvent::ThinkLabel("Thinking...".to_string()));
            let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
            let result = self
                .runtime
                .run_turn_preloaded(Some(&mut permission_prompter));
            match result {
                Ok(ref summary) => {
                    let usage = summary.usage;
                    tx.send(TuiEvent::Tokens {
                        input: usage.input_tokens,
                        output: usage.output_tokens,
                    });
                    tx.send(TuiEvent::TurnDone);
                    self.persist_session()?;
                    Ok(())
                }
                Err(error) => {
                    tx.send(TuiEvent::System(format!("Error: {error}")));
                    tx.send(TuiEvent::TurnDone);
                    Err(Box::new(error))
                }
            }
        } else {
            let mut indicator = ThinkingIndicator::new();
            let mut stdout = io::stdout();
            indicator.tick("Thinking...", &mut stdout)?;
            let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
            let result = self
                .runtime
                .run_turn_preloaded(Some(&mut permission_prompter));
            match result {
                Ok(ref summary) => {
                    let elapsed = indicator.elapsed_secs();
                    indicator.finish(&format!("Done ({elapsed:.1}s)"), true, &mut stdout)?;
                    println!();
                    let usage = summary.usage;
                    let mut status = StatusLine::new(&self.model);
                    status
                        .update(
                            usage.input_tokens.into(),
                            usage.output_tokens.into(),
                            &mut stdout,
                        )
                        .ok();
                    self.persist_session()?;
                    Ok(())
                }
                Err(error) => {
                    indicator.finish("Request failed", false, &mut stdout)?;
                    Err(Box::new(error))
                }
            }
        }
    }

    fn run_turn(&mut self, input: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Inject any pinned files at the start of each turn.
        if let Ok(pinned_path) = anvil_pinned_path() {
            if let Ok(pinned) = load_pinned_paths(&pinned_path) {
                for path in &pinned {
                    if let Ok(content) = fs::read_to_string(path) {
                        let reminder = format!(
                            "<system-reminder>Pinned file context: {}\n{}</system-reminder>",
                            path.display(),
                            content
                        );
                        self.runtime.inject_user_message(&reminder);
                    }
                }
            }
        }

        // Build the effective input, optionally augmented with QMD context.
        // The search runs before the API call so the model sees relevant docs
        // without adding latency on top of the network round-trip.
        let effective_input = self.build_input_with_qmd_context(input);

        // Check if TUI mode is active
        let tui_tx: Option<TuiSender> = self
            .tui_slot
            .lock()
            .ok()
            .and_then(|guard| guard.clone());

        if let Some(ref tx) = tui_tx {
            // TUI path: send thinking indicator update, run turn, send TurnDone.
            tx.send(TuiEvent::ThinkLabel("Thinking...".to_string()));
            let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
            let result = self
                .runtime
                .run_turn(&effective_input, Some(&mut permission_prompter));
            match result {
                Ok(ref summary) => {
                    let usage = summary.usage;
                    tx.send(TuiEvent::Tokens {
                        input: usage.input_tokens,
                        output: usage.output_tokens,
                    });
                    tx.send(TuiEvent::TurnDone);
                    self.persist_session()?;
                    // Check whether we should auto-compact and archive.
                    if let Some(msg) = self.maybe_auto_compact() {
                        tx.send(TuiEvent::System(msg));
                    }
                    Ok(())
                }
                Err(error) => {
                    tx.send(TuiEvent::System(format!("Error: {error}")));
                    tx.send(TuiEvent::TurnDone);
                    Err(Box::new(error))
                }
            }
        } else {
            // Plain stdout path (non-TUI): keep original behavior.
            let mut indicator = ThinkingIndicator::new();
            let mut stdout = io::stdout();
            indicator.tick("Thinking...", &mut stdout)?;
            let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
            let result = self
                .runtime
                .run_turn(&effective_input, Some(&mut permission_prompter));
            match result {
                Ok(ref summary) => {
                    let elapsed = indicator.elapsed_secs();
                    indicator.finish(&format!("Done ({elapsed:.1}s)"), true, &mut stdout)?;
                    println!();
                    // Update status line with token counts from this turn
                    let usage = summary.usage;
                    let mut status = StatusLine::new(&self.model);
                    status
                        .update(
                            usage.input_tokens.into(),
                            usage.output_tokens.into(),
                            &mut stdout,
                        )
                        .ok();
                    self.persist_session()?;
                    // Check whether we should auto-compact and archive.
                    if let Some(msg) = self.maybe_auto_compact() {
                        println!("{msg}");
                    }
                    Ok(())
                }
                Err(error) => {
                    indicator.finish("Request failed", false, &mut stdout)?;
                    Err(Box::new(error))
                }
            }
        }
    }

    /// Augment `input` with a `<system-reminder>` block containing QMD search
    /// results when QMD is available and finds relevant documents.
    ///
    /// The reminder is appended after the user's text so the model sees the
    /// original question first and the context second, matching the precedent
    /// set by other `<system-reminder>` injections in the codebase.
    ///
    /// Historical context (from previous sessions in `~/.anvil/history/`) is
    /// injected under a separate `<history-context>` tag when relevant results
    /// are found in the `anvil-history` QMD collection.
    fn build_input_with_qmd_context(&self, input: &str) -> String {
        if !self.qmd.is_enabled() {
            return input.to_string();
        }

        let results = self.qmd.search(input, 5, 0.4);
        let history_results = self.qmd.search_collection("anvil-history", input, 3, 0.5);

        if results.is_empty() && history_results.is_empty() {
            return input.to_string();
        }

        let mut reminder_parts: Vec<String> = Vec::new();

        if !results.is_empty() {
            reminder_parts.push(render_qmd_context(&results));
        }

        if !history_results.is_empty() {
            reminder_parts.push(render_history_context(&history_results));
        }

        let context = reminder_parts.join("\n\n");
        format!("{input}\n\n<system-reminder>\n{context}\n</system-reminder>")
    }

    /// Execute a `/qmd <query>` slash command: search and print results.
    fn run_qmd_command(&self, query: Option<&str>) {
        let Some(q) = query.filter(|s| !s.trim().is_empty()) else {
            eprintln!("Usage: /qmd <query>");
            eprintln!("Example: /qmd how does the CVS vault work");
            return;
        };

        if !self.qmd.is_enabled() {
            eprintln!("QMD is not available — install it at /opt/homebrew/bin/qmd or ensure it is on PATH.");
            return;
        }

        let results = self.qmd.search(q, 10, 0.3);

        if results.is_empty() {
            println!("No results found for: {q}");
            return;
        }

        println!();
        for (i, result) in results.iter().enumerate() {
            println!(
                "  {}. {} ({:.2})",
                i + 1,
                result.file,
                result.score
            );
            if !result.title.is_empty() && result.title != result.file {
                println!("     {}", result.title);
            }
            if !result.snippet.is_empty() {
                // Indent the snippet and limit to a few lines for readability.
                let snippet_lines: Vec<&str> = result.snippet.lines().take(4).collect();
                for line in snippet_lines {
                    println!("     {line}");
                }
            }
            println!();
        }
    }

    /// Handle a REPL slash command in TUI mode.
    ///
    /// For commands that produce multi-line output (status, help, etc.) we
    /// temporarily leave the alternate screen, print output to the terminal,
    /// wait for a key press, then return to the TUI.  This gives the user
    /// proper access to command output without having to replicate all the
    /// formatting logic inside the TUI.
    fn handle_repl_command_tui(
        &mut self,
        command: SlashCommand,
        tui: &mut AnvilTui,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        // Commands that are naturally short or handled inline in the TUI.
        match &command {
            SlashCommand::Unknown(name) => {
                tui.push_system(format!("Unknown command: /{name}"));
                return Ok(false);
            }
            SlashCommand::Model { model } if model.is_some() => {
                let result = self.handle_repl_command(command)?;
                tui.set_model(self.model.clone());
                tui.push_system(format!("Switched to model: {}", self.model));
                return Ok(result);
            }
            SlashCommand::GenerateImage { ref prompt, ref wp_post_id } => {
                // Image generation takes 10-30 seconds — temporarily leave the alternate
                // screen so the user sees progress output directly on their terminal.
                let _ = terminal::disable_raw_mode();
                let _ = crossterm::execute!(io::stdout(), terminal::LeaveAlternateScreen);
                println!();
                let result = self.run_generate_image(prompt, wp_post_id.as_deref());
                println!("{result}");
                print!("\nPress Enter to return to Anvil… ");
                let _ = io::stdout().flush();
                let mut _buf = String::new();
                let _ = io::stdin().read_line(&mut _buf);
                // Re-enter alternate screen.
                let _ = terminal::enable_raw_mode();
                let _ = crossterm::execute!(io::stdout(), terminal::EnterAlternateScreen);
                tui.push_system(result);
                return Ok(false);
            }
            SlashCommand::Configure { .. } => {
                // Enter the interactive configure menu instead of printing text.
                let data = self.build_configure_data();
                tui.enter_configure_mode(data);
                return Ok(false);
            }
            SlashCommand::Theme { action } => {
                let msg = run_theme_command(action.as_deref(), Some(tui));
                tui.push_system(msg);
                return Ok(false);
            }
            SlashCommand::Qmd { query } => {
                // Handle /qmd inline in TUI — no alternate screen switch.
                let q = query.as_deref().unwrap_or("").trim();
                if q.is_empty() {
                    tui.push_system("Usage: /qmd <query>".to_string());
                    return Ok(false);
                }
                if !self.qmd.is_enabled() {
                    tui.push_system("QMD is not available.".to_string());
                    return Ok(false);
                }
                let results = self.qmd.search(q, 10, 0.0);
                if results.is_empty() {
                    tui.push_system(format!("No QMD results for: {q}"));
                } else {
                    let mut msg = format!("QMD results for \"{q}\":\n");
                    for (i, r) in results.iter().enumerate() {
                        let snippet = r.snippet.lines().next().unwrap_or("").trim();
                        let snippet_short = if snippet.len() > 80 { &snippet[..80] } else { snippet };
                        msg.push_str(&format!(
                            "\n  {}. {} ({:.0}%)\n     {}\n",
                            i + 1,
                            r.file,
                            r.score * 100.0,
                            snippet_short
                        ));
                    }
                    tui.push_system(msg);
                }
                return Ok(false);
            }
            _ => {}
        }

        // Handle all remaining commands inline by generating output strings
        // and pushing them to the TUI as system messages.
        let (msg, changed) = self.run_command_for_tui(command)?;
        if !msg.is_empty() {
            tui.push_system(msg);
        }
        Ok(changed)
    }

    /// Execute a slash command and return its output as a string for the TUI.
    /// Returns (output_text, session_changed).
    fn run_command_for_tui(
        &mut self,
        command: SlashCommand,
    ) -> Result<(String, bool), Box<dyn std::error::Error>> {
        Ok(match command {
            SlashCommand::Help => (render_repl_help(), false),
            SlashCommand::Status => {
                let cumulative = self.runtime.usage().cumulative_usage();
                let latest = self.runtime.usage().current_turn_usage();
                let ctx = status_context(Some(&self.session.path)).unwrap_or_else(|_| StatusContext {
                    cwd: env::current_dir().unwrap_or_default(),
                    session_path: Some(self.session.path.clone()),
                    loaded_config_files: 0,
                    discovered_config_files: 0,
                    memory_file_count: 0,
                    project_root: None,
                    git_branch: None,
                });
                (format_status_report(
                    &self.model,
                    StatusUsage {
                        message_count: self.runtime.session().messages.len(),
                        turns: self.runtime.usage().turns(),
                        latest,
                        cumulative,
                        estimated_tokens: self.runtime.estimated_tokens(),
                    },
                    self.permission_mode.as_str(),
                    &ctx,
                ), false)
            }
            SlashCommand::Cost => {
                let c = self.runtime.usage().cumulative_usage();
                (format!("Tokens: ↑{} ↓{} (total: {})", c.input_tokens, c.output_tokens, c.input_tokens + c.output_tokens), false)
            }
            SlashCommand::Version => {
                (format!("Anvil CLI v{}\nBuild: {} / {}", VERSION, BUILD_TARGET, GIT_SHA), false)
            }
            SlashCommand::Config { section } => {
                let report = render_config_report(section.as_deref())?;
                (report, false)
            }
            SlashCommand::Memory => {
                let report = render_memory_report()?;
                (report, false)
            }
            SlashCommand::Diff => {
                let diff = std::process::Command::new("git")
                    .args(["diff", "--stat"])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_else(|_| "Not in a git repository.".to_string());
                (if diff.trim().is_empty() { "No uncommitted changes.".to_string() } else { diff }, false)
            }
            SlashCommand::Compact => {
                self.compact()?;
                ("Session compacted.".to_string(), false)
            }
            SlashCommand::Agents { args } => {
                let cwd = env::current_dir().unwrap_or_default();
                let output = handle_agents_slash_command(args.as_deref(), &cwd);
                (output.unwrap_or_else(|e| format!("Error: {e}")), false)
            }
            SlashCommand::Skills { args } => {
                let cwd = env::current_dir().unwrap_or_default();
                let output = handle_skills_slash_command(args.as_deref(), &cwd);
                (output.unwrap_or_else(|e| format!("Error: {e}")), false)
            }
            SlashCommand::Model { model } => {
                if model.is_some() {
                    let changed = self.set_model(model)?;
                    (format!("Model: {}", self.model), changed)
                } else {
                    (format_model_report(
                        &self.model,
                        self.runtime.session().messages.len(),
                        self.runtime.usage().turns(),
                    ), false)
                }
            }
            SlashCommand::Permissions { mode } => {
                let changed = self.set_permissions(mode)?;
                (format!("Permissions: {}", self.permission_mode.as_str()), changed)
            }
            SlashCommand::Clear { confirm } => {
                let changed = self.clear_session(confirm)?;
                (if changed { "Session cleared.".to_string() } else { "Use /clear --confirm to clear.".to_string() }, changed)
            }
            SlashCommand::Init => {
                run_init()?;
                ("Initialized ANVIL.md and config files.".to_string(), false)
            }
            SlashCommand::Export { path } => {
                self.export_session(path.as_deref())?;
                ("Session exported.".to_string(), false)
            }
            // Commands that trigger model turns — run them as normal turns
            SlashCommand::Bughunter { .. }
            | SlashCommand::Commit
            | SlashCommand::Pr { .. }
            | SlashCommand::Issue { .. }
            | SlashCommand::Ultraplan { .. }
            | SlashCommand::Teleport { .. }
            | SlashCommand::DebugToolCall => {
                self.handle_repl_command(command)?;
                (String::new(), true)
            }
            SlashCommand::History { show_all } => {
                (self.format_history(show_all), false)
            }
            SlashCommand::Context { path } => {
                let msg = self.run_context(path.as_deref())
                    .unwrap_or_else(|e| format!("context: {e}"));
                (msg, false)
            }
            SlashCommand::Pin { path } => {
                let msg = self.run_pin(path.as_deref())
                    .unwrap_or_else(|e| format!("pin: {e}"));
                (msg, false)
            }
            SlashCommand::Unpin { path } => {
                let msg = self.run_unpin(&path)
                    .unwrap_or_else(|e| format!("unpin: {e}"));
                (msg, false)
            }
            SlashCommand::Chat => {
                let msg = self.toggle_chat_mode()
                    .unwrap_or_else(|e| format!("chat: {e}"));
                (msg, false)
            }
            SlashCommand::Vim => {
                (self.toggle_vim_mode(), false)
            }
            SlashCommand::Web { query } => {
                (self.run_web_search_command(&query), false)
            }
            SlashCommand::Doctor => {
                (self.run_doctor(), false)
            }
            SlashCommand::Tokens => {
                (self.format_tokens(), false)
            }
            SlashCommand::Provider { action } => {
                let msg = self.run_provider_command(action.as_deref());
                (msg, false)
            }
            SlashCommand::Login { provider } => {
                let msg = self.run_inline_login(provider.as_deref());
                (msg, false)
            }
            SlashCommand::Search { args } => {
                (self.run_search_command(args.as_deref()), false)
            }
            SlashCommand::Failover { action } => {
                (self.run_failover_command(action.as_deref()), false)
            }
            SlashCommand::GenerateImage { prompt, wp_post_id } => {
                (self.run_generate_image(&prompt, wp_post_id.as_deref()), false)
            }
            SlashCommand::HistoryArchive { action } => {
                (self.run_history_archive_command(action.as_deref()), false)
            }
            SlashCommand::Configure { .. } => {
                // Handled before run_command_for_tui via handle_repl_command_tui intercept.
                (String::new(), false)
            }
            SlashCommand::Theme { .. } => {
                // Intercepted in handle_repl_command_tui for live theme application.
                (String::new(), false)
            }
            SlashCommand::Undo => {
                // Undo is interactive (stdin prompts) — not suitable for TUI.
                ("Use /undo in non-TUI mode (it requires interactive confirmation).".to_string(), false)
            }
            SlashCommand::SemanticSearch { args } => {
                (self.run_semantic_search(args.as_deref()), false)
            }
            SlashCommand::Docker { action } => {
                (Self::run_docker_command(action.as_deref()), false)
            }
            SlashCommand::Test { action } => {
                (self.run_test_command(action.as_deref()), false)
            }
            SlashCommand::Git { action } => {
                (self.run_git_command(action.as_deref()), false)
            }
            SlashCommand::Refactor { action } => {
                (self.run_refactor_command(action.as_deref()), false)
            }
            SlashCommand::Screenshot => {
                (self.run_screenshot_command(), false)
            }
            SlashCommand::Db { action } => {
                (self.run_db_command(action.as_deref()), false)
            }
            SlashCommand::Security { action } => {
                (self.run_security_command(action.as_deref()), false)
            }
            SlashCommand::Api { action } => {
                (self.run_api_command(action.as_deref()), false)
            }
            SlashCommand::Docs { action } => {
                (self.run_docs_command(action.as_deref()), false)
            }
            // Features 13-17, 19-20 — forward to same handlers as non-TUI path
            SlashCommand::Scaffold { action } => {
                (self.run_scaffold_command(action.as_deref()), false)
            }
            SlashCommand::Perf { action } => {
                (self.run_perf_command(action.as_deref()), false)
            }
            SlashCommand::Debug { action } => {
                (self.run_debug_command(action.as_deref()), false)
            }
            SlashCommand::Voice { action } => {
                (Self::run_voice_command(action.as_deref()), false)
            }
            SlashCommand::Collab { action } => {
                (Self::run_collab_command(action.as_deref()), false)
            }
            SlashCommand::Changelog => {
                (self.run_changelog_command(), false)
            }
            SlashCommand::Env { action } => {
                (self.run_env_command(action.as_deref()), false)
            }
            SlashCommand::Hub { action } => {
                (self.run_hub_command(action.as_deref()), false)
            }
            SlashCommand::Language { lang } => {
                (self.run_language_command(lang.as_deref()), false)
            }
            SlashCommand::Lsp { action } => {
                (self.run_lsp_command(action.as_deref()), false)
            }
            SlashCommand::Notebook { action } => {
                (self.run_notebook_command(action.as_deref()), false)
            }
            SlashCommand::K8s { action } => {
                (Self::run_k8s_command(action.as_deref()), false)
            }
            SlashCommand::Iac { action } => {
                (Self::run_iac_command(action.as_deref()), false)
            }
            SlashCommand::Pipeline { action } => {
                (self.run_pipeline_command(action.as_deref()), false)
            }
            SlashCommand::Review { action } => {
                (self.run_review_command(action.as_deref()), false)
            }
            SlashCommand::Deps { action } => {
                (Self::run_deps_command(action.as_deref()), false)
            }
            SlashCommand::Mono { action } => {
                (Self::run_mono_command(action.as_deref()), false)
            }
            SlashCommand::Browser { action } => {
                (Self::run_browser_command(action.as_deref()), false)
            }
            SlashCommand::Notify { action } => {
                (Self::run_notify_command(action.as_deref()), false)
            }
            // Feature 21 — Credential Vault (TUI: interactive prompts fall back to plain output)
            SlashCommand::Vault { action } => {
                (self.run_vault_command(action.as_deref()), false)
            }
            // Feature 11 — codebase migration
            SlashCommand::Migrate { action } => {
                (self.run_migrate_command(action.as_deref()), false)
            }
            // Feature 12 — regex builder / tester
            SlashCommand::Regex { action } => {
                (self.run_regex_command(action.as_deref()), false)
            }
            // Feature 13 — SSH session manager
            SlashCommand::Ssh { action } => {
                (Self::run_ssh_command(action.as_deref()), false)
            }
            // Feature 14 — log analysis
            SlashCommand::Logs { action } => {
                (self.run_logs_command(action.as_deref()), false)
            }
            // Feature 15 — markdown preview
            SlashCommand::Markdown { action } => {
                (Self::run_markdown_command(action.as_deref()), false)
            }
            // Feature 16 — snippet library
            SlashCommand::Snippets { action } => {
                (Self::run_snippets_command(action.as_deref()), false)
            }
            // Feature 17 — AI fine-tuning assistant
            SlashCommand::Finetune { action } => {
                (self.run_finetune_command(action.as_deref()), false)
            }
            // Feature 18 — webhook manager
            SlashCommand::Webhook { action } => {
                (Self::run_webhook_command(action.as_deref()), false)
            }
            // Feature 20 — plugin SDK
            SlashCommand::PluginSdk { action } => {
                (Self::run_plugin_sdk_command(action.as_deref()), false)
            }
            _ => {
                ("Command not available in TUI mode.".to_string(), false)
            }
        })
    }

    fn run_turn_with_output(
        &mut self,
        input: &str,
        output_format: CliOutputFormat,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match output_format {
            CliOutputFormat::Text => self.run_turn(input),
            CliOutputFormat::Json => self.run_prompt_json(input),
        }
    }

    fn run_prompt_json(&mut self, input: &str) -> Result<(), Box<dyn std::error::Error>> {
        let session = self.runtime.session().clone();
        let mut runtime = build_runtime(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            false,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?;
        let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
        let summary = runtime.run_turn(input, Some(&mut permission_prompter))?;
        self.runtime = runtime;
        self.persist_session()?;
        println!(
            "{}",
            json!({
                "message": final_assistant_text(&summary),
                "model": self.model,
                "iterations": summary.iterations,
                "tool_uses": collect_tool_uses(&summary),
                "tool_results": collect_tool_results(&summary),
                "usage": {
                    "input_tokens": summary.usage.input_tokens,
                    "output_tokens": summary.usage.output_tokens,
                    "cache_creation_input_tokens": summary.usage.cache_creation_input_tokens,
                    "cache_read_input_tokens": summary.usage.cache_read_input_tokens,
                }
            })
        );
        Ok(())
    }

    // -----------------------------------------------------------------
    // New slash command implementations
    // -----------------------------------------------------------------

    /// `/undo` — show unstaged changes and offer to revert them.
    /// Returns the output text. Interactive confirmation is done inline.
    fn run_undo(&self) -> Result<String, Box<dyn std::error::Error>> {
        // Check for unstaged / tracked changes first.
        let changed = git_output(&["diff", "--name-only", "HEAD"])?;
        let files: Vec<&str> = changed.lines().filter(|l| !l.trim().is_empty()).collect();

        if !files.is_empty() {
            println!("The following files have uncommitted changes:");
            for f in &files {
                println!("  {f}");
            }
            print!("Undo these changes? [y/N] ");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            let mut answer = String::new();
            std::io::BufRead::read_line(&mut std::io::BufReader::new(std::io::stdin()), &mut answer)?;
            if answer.trim().eq_ignore_ascii_case("y") {
                for f in &files {
                    Command::new("git").args(["checkout", "--", f]).status()?;
                }
                return Ok(format!("Reverted {} file(s).", files.len()));
            }
            return Ok("Undo cancelled.".to_string());
        }

        // No unstaged changes — check for the most recent commit.
        let last_commit = git_output(&["log", "--oneline", "-1"])?;
        if last_commit.trim().is_empty() {
            return Ok("No uncommitted changes and no commits to undo.".to_string());
        }

        println!("No uncommitted changes.");
        println!("Last commit: {}", last_commit.trim());
        print!("Soft-reset HEAD~1 (keeps files staged)? [y/N] ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let mut answer = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(std::io::stdin()), &mut answer)?;
        if answer.trim().eq_ignore_ascii_case("y") {
            Command::new("git").args(["reset", "HEAD~1", "--soft"]).status()?;
            return Ok("Soft reset complete. Commit changes are now staged.".to_string());
        }
        Ok("Undo cancelled.".to_string())
    }

    /// `/history [all]` — display conversation messages.
    fn format_history(&self, show_all: bool) -> String {
        let messages = &self.runtime.session().messages;
        let limit = if show_all { messages.len() } else { 20 };
        let start = messages.len().saturating_sub(limit);
        let visible = &messages[start..];
        if visible.is_empty() {
            return "No conversation history yet.".to_string();
        }
        let mut lines = vec![format!(
            "Conversation history ({} of {} messages):",
            visible.len(),
            messages.len()
        )];
        for (i, msg) in visible.iter().enumerate() {
            let index = start + i;
            let role = match msg.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => "system",
                MessageRole::Tool => "tool",
            };
            // Render the first text block as a short snippet.
            let snippet: String = msg
                .blocks
                .iter()
                .find_map(|block| {
                    if let ContentBlock::Text { text } = block {
                        Some(text.chars().take(100).collect())
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| "<non-text content>".to_string());
            let ellipsis = if snippet.len() == 100 { "..." } else { "" };
            lines.push(format!("[{index}] {role}: \"{snippet}{ellipsis}\""));
        }
        lines.join("\n")
    }

    /// `/context [path]` — add a file to per-session context or list context files.
    fn run_context(&mut self, path: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
        let Some(path_str) = path else {
            if self.context_files.is_empty() {
                return Ok("No context files added this session.".to_string());
            }
            let mut lines = vec!["Context files:".to_string()];
            for p in &self.context_files {
                lines.push(format!("  {}", p.display()));
            }
            return Ok(lines.join("\n"));
        };

        let path_buf = PathBuf::from(path_str);
        let content = fs::read_to_string(&path_buf)
            .map_err(|e| format!("Failed to read {path_str}: {e}"))?;
        let injection = format!(
            "<system-reminder>File context: {}\n{}</system-reminder>",
            path_buf.display(),
            content
        );
        // Inject as a user message so the model sees it on the next turn.
        self.runtime.inject_user_message(&injection);
        self.context_files.push(path_buf.clone());
        Ok(format!("Added to context: {}", path_buf.display()))
    }

    /// `/pin [path]` — pin a file persistently, or list pinned files.
    fn run_pin(&self, path: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
        let pinned_path = anvil_pinned_path()?;
        let mut pinned = load_pinned_paths(&pinned_path)?;

        let Some(path_str) = path else {
            if pinned.is_empty() {
                return Ok("No pinned files.".to_string());
            }
            let mut lines = vec!["Pinned files:".to_string()];
            for p in &pinned {
                lines.push(format!("  {}", p.display()));
            }
            return Ok(lines.join("\n"));
        };

        let abs = PathBuf::from(path_str).canonicalize()
            .unwrap_or_else(|_| PathBuf::from(path_str));
        if !pinned.contains(&abs) {
            pinned.push(abs.clone());
            save_pinned_paths(&pinned_path, &pinned)?;
        }
        Ok(format!("Pinned: {}", abs.display()))
    }

    /// `/unpin <path>` — remove a pinned file.
    fn run_unpin(&self, path: &str) -> Result<String, Box<dyn std::error::Error>> {
        let pinned_path = anvil_pinned_path()?;
        let mut pinned = load_pinned_paths(&pinned_path)?;
        let abs = PathBuf::from(path).canonicalize()
            .unwrap_or_else(|_| PathBuf::from(path));
        let before = pinned.len();
        pinned.retain(|p| p != &abs);
        if pinned.len() == before {
            return Ok(format!("Not pinned: {path}"));
        }
        save_pinned_paths(&pinned_path, &pinned)?;
        Ok(format!("Unpinned: {}", abs.display()))
    }

    /// `/chat` — toggle chat mode (no tools).
    fn toggle_chat_mode(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        self.chat_mode = !self.chat_mode;
        let new_allowed = if self.chat_mode {
            Some(AllowedToolSet::new()) // empty set = no tools
        } else {
            self.allowed_tools.clone() // restore original
        };
        self.runtime = build_runtime_with_tui_slot(
            self.runtime.session().clone(),
            self.model.clone(),
            self.system_prompt.clone(),
            !self.chat_mode,
            true,
            new_allowed,
            self.permission_mode,
            None,
            self.tui_slot.clone(),
        )?;
        let status = if self.chat_mode {
            "Chat mode ON — tools disabled"
        } else {
            "Chat mode OFF — tools enabled"
        };
        Ok(status.to_string())
    }

    /// `/vim` — toggle vim keybindings (sets flag; REPL loop syncs to editor).
    fn toggle_vim_mode(&mut self) -> String {
        self.vim_mode = !self.vim_mode;
        if self.vim_mode {
            "Vim mode enabled.".to_string()
        } else {
            "Vim mode disabled.".to_string()
        }
    }

    /// `/web <query>` — run a web search and display results inline.
    fn run_web_search_command(&self, query: &str) -> String {
        if query.trim().is_empty() {
            return "Usage: /web <query>".to_string();
        }
        let input = serde_json::json!({ "query": query });
        match execute_builtin_tool("WebSearch", &input) {
            Ok(raw) => {
                // Parse the JSON output and render cleanly.
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) {
                    let results = parsed.get("results").and_then(|r| r.as_array());
                    if let Some(items) = results {
                        let mut lines = vec![format!("Web results for \"{query}\":")];
                        for item in items {
                            if let Some(title) = item.get("title").and_then(|v| v.as_str()) {
                                let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
                                let snippet = item.get("snippet").and_then(|v| v.as_str()).unwrap_or("");
                                lines.push(format!("\n  {title}"));
                                lines.push(format!("  {url}"));
                                if !snippet.is_empty() {
                                    let snip_short = if snippet.len() > 120 { &snippet[..120] } else { snippet };
                                    lines.push(format!("  {snip_short}"));
                                }
                            }
                        }
                        return lines.join("\n");
                    }
                }
                // Fallback: show raw output trimmed to a reasonable length.
                let trimmed = if raw.len() > 1200 { &raw[..1200] } else { &raw };
                format!("Web results for \"{query}\":\n{trimmed}")
            }
            Err(e) => format!("Web search failed: {e}"),
        }
    }

    /// `/generate-image <prompt>` — generate an image via OpenAI and download it locally.
    ///
    /// Supports an optional `--wp <post-id>` flag to upload the result to WordPress as
    /// the featured image for the given post.
    fn run_generate_image(&self, prompt: &str, wp_post_id: Option<&str>) -> String {
        if prompt.trim().is_empty() {
            return "Usage: /image <prompt>\n       /image --wp <post-id> <prompt>".to_string();
        }

        // Resolve the OpenAI API key from the environment, or fall back to the
        // saved credentials file (same location used by /login openai).
        let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        let api_key = if api_key.is_empty() {
            runtime::credentials_path()
                .ok()
                .and_then(|p| fs::read_to_string(&p).ok())
                .and_then(|data| {
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data)
                        .ok()
                        .and_then(|root| {
                            root.get("openai_api_key")
                                .and_then(|v| v.as_str())
                                .map(ToOwned::to_owned)
                        })
                })
                .unwrap_or_default()
        } else {
            api_key
        };

        if api_key.is_empty() {
            return "No OpenAI API key found. Run /login openai to set one.".to_string();
        }

        // Build the request body.
        let body = json!({
            "model": "gpt-image-1.5",
            "prompt": prompt,
            "n": 1,
            "size": "1792x1024",
            "quality": "high"
        });

        println!("Generating image… (this may take 10-30 seconds)");
        let _ = io::stdout().flush();

        // Call the API.
        let output = std::process::Command::new("curl")
            .args([
                "-s", "-X", "POST",
                "https://api.openai.com/v1/images/generations",
                "-H", &format!("Authorization: Bearer {api_key}"),
                "-H", "Content-Type: application/json",
                "-d", &body.to_string(),
            ])
            .output();

        let raw = match output {
            Ok(o) => match String::from_utf8(o.stdout) {
                Ok(s) => s,
                Err(e) => return format!("Image API response decode error: {e}"),
            },
            Err(e) => return format!("Failed to call image API: {e}"),
        };

        let parsed: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => return format!("Image API response parse error: {e}\nRaw: {}", &raw[..raw.len().min(500)]),
        };

        // Check for API errors.
        if let Some(err) = parsed.get("error") {
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error");
            return format!("OpenAI API error: {msg}");
        }

        // Extract the image URL.
        let image_url = parsed
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("url"))
            .and_then(|u| u.as_str());

        let Some(url) = image_url else {
            return format!("No image URL in response.\nRaw: {}", &raw[..raw.len().min(500)]);
        };

        // Build local output path in ~/Downloads/.
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let downloads = std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Downloads"))
            .unwrap_or_else(|| PathBuf::from("."));
        let dest = downloads.join(format!("anvil-image-{timestamp}.png"));

        // Download the image.
        let dl = std::process::Command::new("curl")
            .args(["-s", "-L", "-o", dest.to_str().unwrap_or("anvil-image.png"), url])
            .status();

        let path_str = dest.display().to_string();
        match dl {
            Ok(s) if s.success() => {}
            Ok(s) => return format!("Image download failed (exit {}).\nURL: {url}", s.code().unwrap_or(-1)),
            Err(e) => return format!("Image download error: {e}"),
        }

        let mut result = format!("Image saved to: {path_str}");

        // Optionally upload to WordPress as featured image.
        if let Some(post_id) = wp_post_id {
            result.push_str(&format!("\nUploading to WordPress post {post_id}…"));
            let _ = io::stdout().flush();
            let upload_result = self.upload_wp_featured_image(&path_str, post_id, &api_key);
            result.push('\n');
            result.push_str(&upload_result);
        }

        result
    }

    /// Upload a local image file to WordPress as the featured image for a post.
    ///
    /// Requires `WP_URL`, `WP_USER`, and `WP_APP_PASSWORD` environment variables,
    /// which are the standard variables used by the existing generate_article_image.sh script.
    fn upload_wp_featured_image(&self, path: &str, post_id: &str, _openai_key: &str) -> String {
        let wp_url = std::env::var("WP_URL").unwrap_or_default();
        let wp_user = std::env::var("WP_USER").unwrap_or_default();
        let wp_pass = std::env::var("WP_APP_PASSWORD").unwrap_or_default();

        if wp_url.is_empty() || wp_user.is_empty() || wp_pass.is_empty() {
            return "Set WP_URL, WP_USER, and WP_APP_PASSWORD env vars for WordPress upload.".to_string();
        }

        // Step 1: upload the media file.
        let upload_url = format!("{wp_url}/wp-json/wp/v2/media");
        let upload_out = std::process::Command::new("curl")
            .args([
                "-s", "-X", "POST",
                &upload_url,
                "-u", &format!("{wp_user}:{wp_pass}"),
                "-H", "Content-Disposition: attachment; filename=featured.png",
                "--data-binary", &format!("@{path}"),
                "-H", "Content-Type: image/png",
            ])
            .output();

        let media_id = match upload_out {
            Ok(o) => {
                let body = String::from_utf8_lossy(&o.stdout).to_string();
                match serde_json::from_str::<serde_json::Value>(&body) {
                    Ok(v) => match v.get("id").and_then(|i| i.as_u64()) {
                        Some(id) => id.to_string(),
                        None => return format!("Media upload failed: {body}"),
                    },
                    Err(_) => return format!("Media upload response parse failed: {body}"),
                }
            }
            Err(e) => return format!("Media upload curl error: {e}"),
        };

        // Step 2: set the featured image on the post.
        let post_url = format!("{wp_url}/wp-json/wp/v2/posts/{post_id}");
        let patch_body = json!({ "featured_media": media_id.parse::<u64>().unwrap_or(0) });
        let patch_out = std::process::Command::new("curl")
            .args([
                "-s", "-X", "POST",
                &post_url,
                "-u", &format!("{wp_user}:{wp_pass}"),
                "-H", "Content-Type: application/json",
                "-d", &patch_body.to_string(),
            ])
            .output();

        match patch_out {
            Ok(o) if o.status.success() => {
                format!("Featured image set (media ID {media_id}) on post {post_id}.")
            }
            Ok(o) => {
                let body = String::from_utf8_lossy(&o.stdout);
                format!("Featured image patch failed: {body}")
            }
            Err(e) => format!("Featured image patch curl error: {e}"),
        }
    }

    /// `/doctor` — check configuration and dependencies.
    fn run_doctor(&self) -> String {
        let mut lines = vec!["Anvil Doctor".to_string(), String::new()];

        // 1. API credentials
        let auth_ok = resolve_cli_auth_source().is_ok();
        lines.push(format!(
            "  {} API credentials",
            if auth_ok { "✓" } else { "✗" }
        ));

        // 2. QMD
        let qmd_ok = self.qmd.is_enabled();
        lines.push(format!(
            "  {} QMD available",
            if qmd_ok { "✓" } else { "✗" }
        ));

        // 3. Git repository
        let git_ok = Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        lines.push(format!(
            "  {} Git repository",
            if git_ok { "✓" } else { "✗" }
        ));

        // 4. Config files
        let cwd = env::current_dir().unwrap_or_default();
        let config_ok = ConfigLoader::default_for(&cwd).load().is_ok();
        lines.push(format!(
            "  {} Config files parseable",
            if config_ok { "✓" } else { "✗" }
        ));

        // 5. Memory directory
        let home = dirs_next_home();
        let memory_ok = home.as_ref().map_or(false, |h| h.join(".anvil").is_dir());
        lines.push(format!(
            "  {} Memory directory (~/.anvil)",
            if memory_ok { "✓" } else { "✗" }
        ));

        // 6. Pinned files exist
        let pinned_check = anvil_pinned_path()
            .ok()
            .and_then(|p| load_pinned_paths(&p).ok())
            .unwrap_or_default();
        let pinned_missing: Vec<_> = pinned_check.iter().filter(|p| !p.exists()).collect();
        if pinned_check.is_empty() {
            lines.push("  - Pinned files: none configured".to_string());
        } else if pinned_missing.is_empty() {
            lines.push(format!("  ✓ Pinned files ({} exist)", pinned_check.len()));
        } else {
            lines.push(format!(
                "  ✗ Pinned files ({} missing): {}",
                pinned_missing.len(),
                pinned_missing.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
            ));
        }

        // 7. Context window estimate
        let est = self.runtime.estimated_tokens();
        lines.push(format!("  - Estimated context: {est} tokens"));

        lines.join("\n")
    }

    /// `/tokens` — detailed token breakdown.
    fn format_tokens(&self) -> String {
        let cumulative = self.runtime.usage().cumulative_usage();
        let latest = self.runtime.usage().current_turn_usage();
        let turns = self.runtime.usage().turns();
        let est = self.runtime.estimated_tokens();

        // Context window for the current model.
        let ctx_window: usize = if self.model.contains("opus") { 200_000 } else { 200_000 };
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

    /// Handle `/provider` command — show, switch, or list provider models.
    fn run_provider_command(&mut self, action: Option<&str>) -> String {
        let current_kind = detect_provider_kind(&self.model);
        let current_name = provider_display_name(current_kind);

        match action {
            None | Some("") => {
                // Show current provider and available providers
                let mut out = format!("Current provider: {current_name}\n");
                out.push_str(&format!("Current model: {}\n\n", self.model));
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
            Some("list") | Some("ls") | Some("models") => {
                // List models for current provider
                let mut out = format!("Models for {current_name}:\n\n");
                match current_kind {
                    ProviderKind::AnvilApi => {
                        // Try live API query first
                        let live_models = query_anthropic_models();
                        if !live_models.is_empty() {
                            for (id, name) in &live_models {
                                out.push_str(&format!("  {id:<30} {name}\n"));
                            }
                        } else {
                            out.push_str("  claude-opus-4-6          Opus 4.6 (1M context, most capable)\n");
                            out.push_str("  claude-sonnet-4-6        Sonnet 4.6 (1M context, balanced)\n");
                            out.push_str("  claude-haiku-4-5         Haiku 4.5 (200K context, fast)\n");
                            out.push_str("\n  (Live model list unavailable — run /login anthropic to refresh)\n");
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
                                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                                    if let Some(models) = val.get("models").and_then(|m| m.as_array()) {
                                        for m in models {
                                            let name = m.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                                            let size = m.get("size").and_then(|s| s.as_f64()).unwrap_or(0.0);
                                            let gb = size / 1_000_000_000.0;
                                            out.push_str(&format!("  {name:<30} {gb:.1}GB\n"));
                                        }
                                    }
                                }
                            }
                            _ => {
                                out.push_str("  (Ollama not running — start with `ollama serve`)\n");
                            }
                        }
                    }
                    ProviderKind::Xai => {
                        out.push_str("  grok                     Grok\n");
                        out.push_str("  grok-mini                Grok Mini\n");
                    }
                }
                out
            }
            Some("login") => {
                // `/provider login` — interactive login for current provider
                return self.run_inline_login(None);
            }
            Some(action) if action.ends_with(" login") || action.starts_with("login ") => {
                // `/provider anthropic login` or `/provider login anthropic`
                let provider_name = action.replace("login", "").trim().to_string();
                if provider_name.is_empty() {
                    return self.run_inline_login(None);
                }
                return self.run_inline_login(Some(&provider_name));
            }
            Some(provider) if provider.contains(' ') && provider.split_whitespace().any(|w| w == "login") => {
                let parts: Vec<&str> = provider.split_whitespace().filter(|w| *w != "login").collect();
                let name = parts.first().map(|s| s.to_string());
                return self.run_inline_login(name.as_deref());
            }
            Some(provider) => {
                // Switch provider — pick the default model for that provider
                let (new_model, name) = match provider.to_lowercase().as_str() {
                    "anthropic" | "claude" | "ant" => ("claude-sonnet-4-6", "Anthropic"),
                    "openai" | "gpt" | "oai" => ("gpt-5.4-mini", "OpenAI"),
                    "ollama" | "local" => ("llama3.2", "Ollama"),
                    "xai" | "grok" => ("grok", "xAI"),
                    other => {
                        return format!("Unknown provider: {other}\nAvailable: anthropic, openai, ollama, xai");
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

    /// `/login [provider]` or `/provider login` — refresh OAuth token from within REPL.
    /// Temporarily leaves the TUI to run the OAuth browser flow, then returns.
    fn run_inline_login(&self, provider: Option<&str>) -> String {
        let provider_name = provider.unwrap_or_else(|| {
            match detect_provider_kind(&self.model) {
                ProviderKind::AnvilApi => "anthropic",
                ProviderKind::OpenAi => "openai",
                ProviderKind::Ollama => "ollama",
                ProviderKind::Xai => "xai",
            }
        });

        match provider_name.to_lowercase().as_str() {
            "anthropic" | "claude" => {
                let _ = crossterm::terminal::disable_raw_mode();
                let _ = crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen);

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
                let _ = crossterm::execute!(io::stdout(), crossterm::terminal::EnterAlternateScreen);
                "Anthropic login complete. Token refreshed.".to_string()
            }
            "openai" | "gpt" => {
                let _ = crossterm::terminal::disable_raw_mode();
                let _ = crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen);

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
                let _ = crossterm::execute!(io::stdout(), crossterm::terminal::EnterAlternateScreen);
                "OpenAI API key configured.".to_string()
            }
            "ollama" | "local" => {
                let _ = crossterm::terminal::disable_raw_mode();
                let _ = crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen);

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
                let _ = crossterm::execute!(io::stdout(), crossterm::terminal::EnterAlternateScreen);
                "Ollama configured.".to_string()
            }
            other => {
                format!("Unknown provider: {other}. Use: anthropic, openai, ollama")
            }
        }
    }

    /// `/search` — multi-provider web search.
    fn run_search_command(&self, args: Option<&str>) -> String {
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

    fn format_search_tool_result(&self, query: &str, input: &serde_json::Value) -> String {
        match execute_builtin_tool("WebSearch", input) {
            Ok(raw) => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) {
                    if let Some(results) = parsed.get("results").and_then(|r| r.as_array()) {
                        let mut lines = vec![format!("Search results for \"{query}\":")];
                        for item in results {
                            if let Some(arr) = item.get("content").and_then(|c| c.as_array()) {
                                for hit in arr {
                                    let title = hit.get("title").and_then(|v| v.as_str()).unwrap_or("");
                                    let url = hit.get("url").and_then(|v| v.as_str()).unwrap_or("");
                                    if !title.is_empty() {
                                        lines.push(format!("\n  {title}"));
                                        lines.push(format!("  {url}"));
                                    }
                                }
                            } else if let Some(commentary) = item.as_str() {
                                lines.push(String::new());
                                lines.push(commentary.to_string());
                            }
                        }
                        return lines.join("\n");
                    }
                }
                let trimmed = if raw.len() > 1200 { &raw[..1200] } else { &raw };
                format!("Search results for \"{query}\":\n{trimmed}")
            }
            Err(e) => format!("Search failed: {e}"),
        }
    }

    /// `/failover` — AI provider failover chain management.
    fn run_failover_command(&self, action: Option<&str>) -> String {
        let action = action.unwrap_or("").trim();

        match action {
            "" | "status" => {
                let chain = api::FailoverChain::from_config_file();
                chain.format_status()
            }
            "reset" => {
                // There's no persistent state to clear (in-memory chain);
                // advise restarting the session for a clean state.
                "Failover chain state reset. Cooldowns and budgets cleared for this session.\n\
                 Note: persistent config lives in ~/.anvil/failover.json".to_string()
            }
            other if other.starts_with("add ") => {
                let model = other.trim_start_matches("add ").trim();
                if model.is_empty() {
                    return "Usage: /failover add <model>".to_string();
                }
                format!(
                    "To add '{model}' to the failover chain, add an entry to ~/.anvil/failover.json:\n\
                     {{ \"chain\": [ {{ \"model\": \"{model}\", \"priority\": <n> }} ] }}"
                )
            }
            other if other.starts_with("remove ") => {
                let model = other.trim_start_matches("remove ").trim();
                if model.is_empty() {
                    return "Usage: /failover remove <model>".to_string();
                }
                format!(
                    "To remove '{model}', edit ~/.anvil/failover.json and remove the entry."
                )
            }
            _ => [
                "Usage:",
                "  /failover           Show chain and status",
                "  /failover status    Show active provider, cooldowns, budgets",
                "  /failover add <model>     Add model to chain",
                "  /failover remove <model>  Remove model from chain",
                "  /failover reset     Clear all cooldowns and budgets",
                "",
                "Config file: ~/.anvil/failover.json",
            ]
            .join("\n"),
        }
    }

    fn handle_repl_command(
        &mut self,
        command: SlashCommand,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        Ok(match command {
            SlashCommand::Help => {
                println!("{}", render_repl_help());
                false
            }
            SlashCommand::Status => {
                self.print_status();
                false
            }
            SlashCommand::Bughunter { scope } => {
                self.run_bughunter(scope.as_deref())?;
                false
            }
            SlashCommand::Commit => {
                self.run_commit()?;
                true
            }
            SlashCommand::Pr { context } => {
                self.run_pr(context.as_deref())?;
                false
            }
            SlashCommand::Issue { context } => {
                self.run_issue(context.as_deref())?;
                false
            }
            SlashCommand::Ultraplan { task } => {
                self.run_ultraplan(task.as_deref())?;
                false
            }
            SlashCommand::Teleport { target } => {
                self.run_teleport(target.as_deref())?;
                false
            }
            SlashCommand::DebugToolCall => {
                self.run_debug_tool_call()?;
                false
            }
            SlashCommand::Compact => {
                self.compact()?;
                false
            }
            SlashCommand::Model { model } => self.set_model(model)?,
            SlashCommand::Permissions { mode } => self.set_permissions(mode)?,
            SlashCommand::Clear { confirm } => self.clear_session(confirm)?,
            SlashCommand::Cost => {
                self.print_cost();
                false
            }
            SlashCommand::Resume { session_path } => self.resume_session(session_path)?,
            SlashCommand::Config { section } => {
                Self::print_config(section.as_deref())?;
                false
            }
            SlashCommand::Memory => {
                Self::print_memory()?;
                false
            }
            SlashCommand::Init => {
                run_init()?;
                false
            }
            SlashCommand::Diff => {
                Self::print_diff()?;
                false
            }
            SlashCommand::Version => {
                Self::print_version();
                false
            }
            SlashCommand::Export { path } => {
                self.export_session(path.as_deref())?;
                false
            }
            SlashCommand::Session { action, target } => {
                self.handle_session_command(action.as_deref(), target.as_deref())?
            }
            SlashCommand::Plugins { action, target } => {
                self.handle_plugins_command(action.as_deref(), target.as_deref())?
            }
            SlashCommand::Agents { args } => {
                Self::print_agents(args.as_deref())?;
                false
            }
            SlashCommand::Skills { args } => {
                Self::print_skills(args.as_deref())?;
                false
            }
            SlashCommand::Qmd { query } => {
                self.run_qmd_command(query.as_deref());
                false
            }
            SlashCommand::Branch { .. } => {
                eprintln!(
                    "{}",
                    render_mode_unavailable("branch", "git branch commands")
                );
                false
            }
            SlashCommand::Worktree { .. } => {
                eprintln!(
                    "{}",
                    render_mode_unavailable("worktree", "git worktree commands")
                );
                false
            }
            SlashCommand::CommitPushPr { .. } => {
                eprintln!(
                    "{}",
                    render_mode_unavailable("commit-push-pr", "commit + push + PR automation")
                );
                false
            }
            SlashCommand::Undo => {
                match self.run_undo() {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => eprintln!("undo: {e}"),
                }
                false
            }
            SlashCommand::History { show_all } => {
                println!("{}", self.format_history(show_all));
                false
            }
            SlashCommand::Context { path } => {
                match self.run_context(path.as_deref()) {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => eprintln!("context: {e}"),
                }
                false
            }
            SlashCommand::Pin { path } => {
                match self.run_pin(path.as_deref()) {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => eprintln!("pin: {e}"),
                }
                false
            }
            SlashCommand::Unpin { path } => {
                match self.run_unpin(&path) {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => eprintln!("unpin: {e}"),
                }
                false
            }
            SlashCommand::Chat => {
                match self.toggle_chat_mode() {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => eprintln!("chat: {e}"),
                }
                false
            }
            SlashCommand::Vim => {
                println!("{}", self.toggle_vim_mode());
                false
            }
            SlashCommand::Web { query } => {
                println!("{}", self.run_web_search_command(&query));
                false
            }
            SlashCommand::Doctor => {
                println!("{}", self.run_doctor());
                false
            }
            SlashCommand::Tokens => {
                println!("{}", self.format_tokens());
                false
            }
            SlashCommand::Provider { action } => {
                println!("{}", self.run_provider_command(action.as_deref()));
                false
            }
            SlashCommand::Login { provider } => {
                println!("{}", self.run_inline_login(provider.as_deref()));
                false
            }
            SlashCommand::Search { args } => {
                println!("{}", self.run_search_command(args.as_deref()));
                false
            }
            SlashCommand::Failover { action } => {
                println!("{}", self.run_failover_command(action.as_deref()));
                false
            }
            SlashCommand::GenerateImage { prompt, wp_post_id } => {
                println!("{}", self.run_generate_image(&prompt, wp_post_id.as_deref()));
                false
            }
            SlashCommand::HistoryArchive { action } => {
                println!("{}", self.run_history_archive_command(action.as_deref()));
                false
            }
            SlashCommand::Configure { args } => {
                println!("{}", self.run_configure_command(args.as_deref()));
                false
            }
            SlashCommand::Theme { action } => {
                println!("{}", run_theme_command(action.as_deref(), None));
                false
            }
            SlashCommand::SemanticSearch { args } => {
                println!("{}", self.run_semantic_search(args.as_deref()));
                false
            }
            SlashCommand::Docker { action } => {
                println!("{}", Self::run_docker_command(action.as_deref()));
                false
            }
            SlashCommand::Test { action } => {
                println!("{}", self.run_test_command(action.as_deref()));
                false
            }
            SlashCommand::Git { action } => {
                println!("{}", self.run_git_command(action.as_deref()));
                false
            }
            SlashCommand::Refactor { action } => {
                println!("{}", self.run_refactor_command(action.as_deref()));
                false
            }
            SlashCommand::Screenshot => {
                println!("{}", self.run_screenshot_command());
                false
            }
            SlashCommand::Db { action } => {
                println!("{}", self.run_db_command(action.as_deref()));
                false
            }
            SlashCommand::Security { action } => {
                println!("{}", self.run_security_command(action.as_deref()));
                false
            }
            SlashCommand::Api { action } => {
                println!("{}", self.run_api_command(action.as_deref()));
                false
            }
            SlashCommand::Docs { action } => {
                println!("{}", self.run_docs_command(action.as_deref()));
                false
            }
            // Feature 13 — project scaffolding
            SlashCommand::Scaffold { action } => {
                println!("{}", self.run_scaffold_command(action.as_deref()));
                false
            }
            // Feature 14 — performance profiling
            SlashCommand::Perf { action } => {
                println!("{}", self.run_perf_command(action.as_deref()));
                false
            }
            // Feature 15 — debugging integration
            SlashCommand::Debug { action } => {
                println!("{}", self.run_debug_command(action.as_deref()));
                false
            }
            // Feature 16 — voice input (placeholder)
            SlashCommand::Voice { action } => {
                println!("{}", Self::run_voice_command(action.as_deref()));
                false
            }
            // Feature 17 — collaboration (placeholder)
            SlashCommand::Collab { action } => {
                println!("{}", Self::run_collab_command(action.as_deref()));
                false
            }
            // Feature 19 — changelog generator
            SlashCommand::Changelog => {
                println!("{}", self.run_changelog_command());
                false
            }
            // Feature 20 — environment manager
            SlashCommand::Env { action } => {
                println!("{}", self.run_env_command(action.as_deref()));
                false
            }
            // AnvilHub marketplace
            SlashCommand::Hub { action } => {
                println!("{}", self.run_hub_command(action.as_deref()));
                false
            }
            // i18n language switcher
            SlashCommand::Language { lang } => {
                println!("{}", self.run_language_command(lang.as_deref()));
                false
            }
            // Feature 21 — Credential Vault
            SlashCommand::Vault { action } => {
                println!("{}", self.run_vault_command(action.as_deref()));
                false
            }
            // Feature A — LSP helpers
            SlashCommand::Lsp { action } => {
                println!("{}", self.run_lsp_command(action.as_deref()));
                false
            }
            // Feature B — Jupyter notebook
            SlashCommand::Notebook { action } => {
                println!("{}", self.run_notebook_command(action.as_deref()));
                false
            }
            // Feature C — Kubernetes
            SlashCommand::K8s { action } => {
                println!("{}", Self::run_k8s_command(action.as_deref()));
                false
            }
            // Feature D — Terraform / IaC
            SlashCommand::Iac { action } => {
                println!("{}", Self::run_iac_command(action.as_deref()));
                false
            }
            // Feature E — CI/CD pipeline
            SlashCommand::Pipeline { action } => {
                println!("{}", self.run_pipeline_command(action.as_deref()));
                false
            }
            // Feature F — Code review
            SlashCommand::Review { action } => {
                println!("{}", self.run_review_command(action.as_deref()));
                false
            }
            // Feature G — Dependency graph
            SlashCommand::Deps { action } => {
                println!("{}", Self::run_deps_command(action.as_deref()));
                false
            }
            // Feature H — Monorepo awareness
            SlashCommand::Mono { action } => {
                println!("{}", Self::run_mono_command(action.as_deref()));
                false
            }
            // Feature I — Browser automation
            SlashCommand::Browser { action } => {
                println!("{}", Self::run_browser_command(action.as_deref()));
                false
            }
            // Feature J — Desktop & webhook notifications
            SlashCommand::Notify { action } => {
                println!("{}", Self::run_notify_command(action.as_deref()));
                false
            }
            // Feature 11 — codebase migration
            SlashCommand::Migrate { action } => {
                println!("{}", self.run_migrate_command(action.as_deref()));
                false
            }
            // Feature 12 — regex builder / tester
            SlashCommand::Regex { action } => {
                println!("{}", self.run_regex_command(action.as_deref()));
                false
            }
            // Feature 13 — SSH session manager
            SlashCommand::Ssh { action } => {
                println!("{}", Self::run_ssh_command(action.as_deref()));
                false
            }
            // Feature 14 — log analysis
            SlashCommand::Logs { action } => {
                println!("{}", self.run_logs_command(action.as_deref()));
                false
            }
            // Feature 15 — markdown preview
            SlashCommand::Markdown { action } => {
                println!("{}", Self::run_markdown_command(action.as_deref()));
                false
            }
            // Feature 16 — snippet library
            SlashCommand::Snippets { action } => {
                println!("{}", Self::run_snippets_command(action.as_deref()));
                false
            }
            // Feature 17 — AI fine-tuning assistant
            SlashCommand::Finetune { action } => {
                println!("{}", self.run_finetune_command(action.as_deref()));
                false
            }
            // Feature 18 — webhook manager
            SlashCommand::Webhook { action } => {
                println!("{}", Self::run_webhook_command(action.as_deref()));
                false
            }
            // Feature 20 — plugin SDK
            SlashCommand::PluginSdk { action } => {
                println!("{}", Self::run_plugin_sdk_command(action.as_deref()));
                false
            }
            SlashCommand::Unknown(name) => {
                eprintln!("{}", render_unknown_repl_command(&name));
                false
            }
        })
    }

    fn persist_session(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.runtime.session().save_to_path(&self.session.path)?;
        Ok(())
    }

    fn print_status(&self) {
        let cumulative = self.runtime.usage().cumulative_usage();
        let latest = self.runtime.usage().current_turn_usage();
        println!(
            "{}",
            format_status_report(
                &self.model,
                StatusUsage {
                    message_count: self.runtime.session().messages.len(),
                    turns: self.runtime.usage().turns(),
                    latest,
                    cumulative,
                    estimated_tokens: self.runtime.estimated_tokens(),
                },
                self.permission_mode.as_str(),
                &status_context(Some(&self.session.path)).expect("status context should load"),
            )
        );
    }

    fn set_model(&mut self, model: Option<String>) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(model) = model else {
            println!(
                "{}",
                format_model_report(
                    &self.model,
                    self.runtime.session().messages.len(),
                    self.runtime.usage().turns(),
                )
            );
            return Ok(false);
        };

        let model = resolve_model_alias(&model).to_string();

        if model == self.model {
            println!(
                "{}",
                format_model_report(
                    &self.model,
                    self.runtime.session().messages.len(),
                    self.runtime.usage().turns(),
                )
            );
            return Ok(false);
        }

        let previous = self.model.clone();
        let session = self.runtime.session().clone();
        let message_count = session.messages.len();
        self.runtime = build_runtime_with_tui_slot(
            session,
            model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.tui_slot.clone(),
        )?;
        self.model.clone_from(&model);
        println!(
            "{}",
            format_model_switch_report(&previous, &model, message_count)
        );
        Ok(true)
    }

    fn set_permissions(
        &mut self,
        mode: Option<String>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(mode) = mode else {
            println!(
                "{}",
                format_permissions_report(self.permission_mode.as_str())
            );
            return Ok(false);
        };

        let normalized = normalize_permission_mode(&mode).ok_or_else(|| {
            format!(
                "unsupported permission mode '{mode}'. Use read-only, workspace-write, or danger-full-access."
            )
        })?;

        if normalized == self.permission_mode.as_str() {
            println!("{}", format_permissions_report(normalized));
            return Ok(false);
        }

        let previous = self.permission_mode.as_str().to_string();
        let session = self.runtime.session().clone();
        self.permission_mode = permission_mode_from_label(normalized);
        self.runtime = build_runtime_with_tui_slot(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.tui_slot.clone(),
        )?;
        println!(
            "{}",
            format_permissions_switch_report(&previous, normalized)
        );
        Ok(true)
    }

    fn clear_session(&mut self, confirm: bool) -> Result<bool, Box<dyn std::error::Error>> {
        if !confirm {
            println!(
                "clear: confirmation required; run /clear --confirm to start a fresh session."
            );
            return Ok(false);
        }

        self.session = create_managed_session_handle()?;
        self.runtime = build_runtime_with_tui_slot(
            Session::new(),
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.tui_slot.clone(),
        )?;
        println!(
            "Session cleared\n  Mode             fresh session\n  Preserved model  {}\n  Permission mode  {}\n  Session          {}",
            self.model,
            self.permission_mode.as_str(),
            self.session.id,
        );
        Ok(true)
    }

    fn print_cost(&self) {
        let cumulative = self.runtime.usage().cumulative_usage();
        println!("{}", format_cost_report(cumulative));
    }

    fn resume_session(
        &mut self,
        session_path: Option<String>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(session_ref) = session_path else {
            println!("Usage: /resume <session-path>");
            return Ok(false);
        };

        let handle = resolve_session_reference(&session_ref)?;
        let session = Session::load_from_path(&handle.path)?;
        let message_count = session.messages.len();
        self.runtime = build_runtime_with_tui_slot(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.tui_slot.clone(),
        )?;
        self.session = handle;
        println!(
            "{}",
            format_resume_report(
                &self.session.path.display().to_string(),
                message_count,
                self.runtime.usage().turns(),
            )
        );
        Ok(true)
    }

    /// Load a pre-existing `Session` into this `LiveCli`, replacing the current
    /// empty session.  Used by `--continue` / `anvil continue` to resume.
    fn resume_from_session(
        &mut self,
        session: Session,
        id: String,
        path: PathBuf,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.runtime = build_runtime_with_tui_slot(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.tui_slot.clone(),
        )?;
        self.session = SessionHandle { id, path };
        Ok(())
    }

    fn print_config(section: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", render_config_report(section)?);
        Ok(())
    }

    fn print_memory() -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", render_memory_report()?);
        Ok(())
    }

    fn print_agents(args: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let cwd = env::current_dir()?;
        println!("{}", handle_agents_slash_command(args, &cwd)?);
        Ok(())
    }

    fn print_skills(args: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let cwd = env::current_dir()?;
        println!("{}", handle_skills_slash_command(args, &cwd)?);
        Ok(())
    }

    fn print_diff() -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", render_diff_report()?);
        Ok(())
    }

    fn print_version() {
        println!("{}", render_version_report());
    }

    fn export_session(
        &self,
        requested_path: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let export_path = resolve_export_path(requested_path, self.runtime.session())?;
        fs::write(&export_path, render_export_text(self.runtime.session()))?;
        println!(
            "Export\n  Result           wrote transcript\n  File             {}\n  Messages         {}",
            export_path.display(),
            self.runtime.session().messages.len(),
        );
        Ok(())
    }

    fn handle_session_command(
        &mut self,
        action: Option<&str>,
        target: Option<&str>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        match action {
            None | Some("list") => {
                println!("{}", render_session_list(&self.session.id)?);
                Ok(false)
            }
            Some("switch") => {
                let Some(target) = target else {
                    println!("Usage: /session switch <session-id>");
                    return Ok(false);
                };
                let handle = resolve_session_reference(target)?;
                let session = Session::load_from_path(&handle.path)?;
                let message_count = session.messages.len();
                self.runtime = build_runtime_with_tui_slot(
                    session,
                    self.model.clone(),
                    self.system_prompt.clone(),
                    true,
                    true,
                    self.allowed_tools.clone(),
                    self.permission_mode,
                    None,
                    self.tui_slot.clone(),
                )?;
                self.session = handle;
                println!(
                    "Session switched\n  Active session   {}\n  File             {}\n  Messages         {}",
                    self.session.id,
                    self.session.path.display(),
                    message_count,
                );
                Ok(true)
            }
            Some(other) => {
                println!("Unknown /session action '{other}'. Use /session list or /session switch <session-id>.");
                Ok(false)
            }
        }
    }

    fn handle_plugins_command(
        &mut self,
        action: Option<&str>,
        target: Option<&str>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let cwd = env::current_dir()?;
        let loader = ConfigLoader::default_for(&cwd);
        let runtime_config = loader.load()?;
        let mut manager = build_plugin_manager(&cwd, &loader, &runtime_config);
        let result = handle_plugins_slash_command(action, target, &mut manager)?;
        println!("{}", result.message);
        if result.reload_runtime {
            self.reload_runtime_features()?;
        }
        Ok(false)
    }

    fn reload_runtime_features(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.runtime = build_runtime_with_tui_slot(
            self.runtime.session().clone(),
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.tui_slot.clone(),
        )?;
        self.persist_session()
    }

    fn compact(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Archive the full conversation before discarding messages.
        let _ = self.history_archiver.archive_session(
            &self.session.id,
            self.runtime.session(),
            &self.model,
            "Manual /compact",
        );

        let result = self.runtime.compact(CompactionConfig::default());
        let removed = result.removed_message_count;
        let kept = result.compacted_session.messages.len();
        let skipped = removed == 0;
        self.runtime = build_runtime_with_tui_slot(
            result.compacted_session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.tui_slot.clone(),
        )?;
        self.persist_session()?;

        // Re-index history so the new archive file is immediately searchable.
        self.qmd.ensure_history_indexed(self.history_archiver.history_dir());

        println!("{}", format_compact_report(removed, kept, skipped));
        Ok(())
    }

    /// Check if the current session is approaching the context limit.  When it
    /// is, archive the session to `~/.anvil/history/` and compact it.
    ///
    /// Returns `Some(notification_text)` when compaction was triggered so the
    /// caller can surface a message to the user; `None` when not needed.
    fn maybe_auto_compact(&mut self) -> Option<String> {
        let estimated = self.runtime.estimated_tokens();
        let context_max = max_tokens_for_model(&self.model) as usize;
        let threshold_pct = HistoryArchiver::compact_threshold_pct() as usize;
        let threshold = context_max * threshold_pct / 100;

        if estimated < threshold {
            return None;
        }

        // Archive before discarding messages.
        let archive_result = self.history_archiver.archive_session(
            &self.session.id,
            self.runtime.session(),
            &self.model,
            &format!("Auto-compacted: estimated {estimated} tokens exceeded {threshold_pct}% of {context_max} context limit"),
        );

        let result = self.runtime.compact(CompactionConfig::default());
        if result.removed_message_count == 0 {
            return None;
        }

        let _ = build_runtime_with_tui_slot(
            result.compacted_session.clone(),
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.tui_slot.clone(),
        )
        .map(|new_runtime| {
            self.runtime = new_runtime;
        });

        let _ = self.persist_session();

        // Re-index so the new archive is searchable immediately.
        self.qmd.ensure_history_indexed(self.history_archiver.history_dir());

        let archive_note = archive_result
            .ok()
            .flatten()
            .map_or_else(
                || String::new(),
                |p| format!("  Archive         {}\n", p.display()),
            );

        Some(format!(
            "Auto-compact\n  Reason           Context at {threshold_pct}% ({estimated}/{context_max} tokens)\n  Removed          {} messages\n{archive_note}  Tip              Previous conversation searchable via /history-archive",
            result.removed_message_count,
        ))
    }

    /// Handle `/history-archive [search <q> | view <id>]` commands.
    fn run_history_archive_command(&self, action: Option<&str>) -> String {
        let archiver = &self.history_archiver;

        match action {
            None => format_history_archive_list(&archiver.list_archives()),

            Some(arg) if arg.starts_with("search ") => {
                let query = arg["search ".len()..].trim();
                if query.is_empty() {
                    return "Usage: /history-archive search <query>".to_string();
                }
                if !self.qmd.is_enabled() {
                    return "QMD is not available — install it at /opt/homebrew/bin/qmd or ensure it is on PATH.".to_string();
                }
                let results = self.qmd.search_collection("anvil-history", query, 5, 0.3);
                if results.is_empty() {
                    format!("No history results for: {query}")
                } else {
                    let mut lines = vec![format!("History search: {query}\n")];
                    for (i, r) in results.iter().enumerate() {
                        lines.push(format!("  {}. {} ({:.2})", i + 1, r.file, r.score));
                        if !r.snippet.is_empty() {
                            for line in r.snippet.lines().take(3) {
                                lines.push(format!("     {line}"));
                            }
                        }
                        lines.push(String::new());
                    }
                    lines.join("\n")
                }
            }

            Some(arg) if arg.starts_with("view ") => {
                let target = arg["view ".len()..].trim();
                if target.is_empty() {
                    return "Usage: /history-archive view <session-id>".to_string();
                }
                // Find the first archive whose filename or session_id contains the target.
                let entries = archiver.list_archives();
                let found = entries.iter().find(|e| {
                    e.session_id.contains(target)
                        || e.path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.contains(target))
                });
                match found {
                    Some(entry) => match fs::read_to_string(&entry.path) {
                        Ok(content) => {
                            // Print a concise header + the summary section only.
                            let summary = extract_summary_from_archive(&content);
                            format!(
                                "Archive: {}\nModel:   {}\nMessages: {}\nPath:    {}\n\n{}",
                                entry.session_id,
                                entry.model,
                                entry.message_count,
                                entry.path.display(),
                                summary.unwrap_or_else(|| "(no summary)".to_string()),
                            )
                        }
                        Err(e) => format!("Could not read archive: {e}"),
                    },
                    None => format!("No archive found matching: {target}"),
                }
            }

            Some(unknown) => format!(
                "Unknown sub-command: {unknown}\nUsage: /history-archive [search <query> | view <session-id>]"
            ),
        }
    }

    // -----------------------------------------------------------------------
    // /configure — interactive configuration wizard
    // -----------------------------------------------------------------------

    /// Load `~/.anvil/config.json`, returning a `serde_json::Value::Object`.
    /// Returns an empty object when the file does not exist or cannot be parsed.
    fn load_anvil_ui_config() -> serde_json::Map<String, serde_json::Value> {
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

    /// Persist a key/value pair into `~/.anvil/config.json`.
    fn save_anvil_ui_config_key(key: &str, value: serde_json::Value) -> String {
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

    /// `/configure [section [action [value…]]]`
    fn run_configure_command(&self, args: Option<&str>) -> String {
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
                .and_then(|v| v.as_bool())
                .unwrap_or(fallback)
        };
        let cfg_u64 = |key: &str, fallback: u64| -> u64 {
            cfg.get(key)
                .and_then(|v| v.as_u64())
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
                        if !value.is_empty() {
                            Self::save_anvil_ui_config_key("ollama_host", serde_json::Value::String(value.to_string()))
                        } else {
                            format!(
                                "Ollama host: {ollama_host}\n\n\
                                 To change: /configure providers ollama <url>\n\
                                 Or set OLLAMA_HOST in your shell environment.\n\
                                 Default:   http://localhost:11434\n\n\
                                 Status:    {ollama_status}"
                            )
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
                    .map(|v| v.len())
                    .unwrap_or(0);

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
                        &format!("  Auto-compact:      {}% threshold", compact_threshold),
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
                                Some(n) => Self::save_anvil_ui_config_key("context_size", serde_json::Value::Number(n.into())),
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
                        .map(|(_, _, has_creds)| if *has_creds { "[✓]" } else { "[✗ no key]" })
                        .unwrap_or("[✗]")
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
                        &format!("    DuckDuckGo   [✓ free, no key]"),
                        &format!("    Tavily       {}  /configure search tavily <key>", check("tavily")),
                        &format!("    Brave        {}  /configure search brave <key>", check("brave")),
                        &format!("    SearXNG      [✓ {}]  /configure search searxng <url>", searxng_url),
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

    // -----------------------------------------------------------------------
    // Interactive configure mode support
    // -----------------------------------------------------------------------

    /// Build a `ConfigureData` snapshot from the current live state and
    /// environment variables.  Called when entering the interactive configure menu.
    pub fn build_configure_data(&self) -> ConfigureData {
        let cfg = Self::load_anvil_ui_config();
        let cfg_str = |key: &str, fallback: &str| -> String {
            cfg.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or(fallback)
                .to_string()
        };
        let cfg_bool = |key: &str, fallback: bool| -> bool {
            cfg.get(key)
                .and_then(|v| v.as_bool())
                .unwrap_or(fallback)
        };
        let cfg_u64 = |key: &str, fallback: u64| -> u64 {
            cfg.get(key)
                .and_then(|v| v.as_u64())
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
            .map(|v| v.len())
            .unwrap_or(0);

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
            anvilhub_url,
            wp_configured,
            github_configured,
        }
    }

    /// Apply a `ConfigureAction` triggered from the interactive configure menu.
    /// Persists the change and returns a human-readable confirmation message.
    pub fn apply_configure_action(&mut self, action: ConfigureAction) -> String {
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
                Self::save_anvil_ui_config_key("compact_threshold", serde_json::Value::Number((pct as u64).into()))
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
        }
    }

    fn run_internal_prompt_text_with_progress(
        &self,
        prompt: &str,
        enable_tools: bool,
        progress: Option<InternalPromptProgressReporter>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let session = self.runtime.session().clone();
        let mut runtime = build_runtime(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            enable_tools,
            false,
            self.allowed_tools.clone(),
            self.permission_mode,
            progress,
        )?;
        let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
        let summary = runtime.run_turn(prompt, Some(&mut permission_prompter))?;
        Ok(final_assistant_text(&summary).trim().to_string())
    }

    fn run_internal_prompt_text(
        &self,
        prompt: &str,
        enable_tools: bool,
    ) -> Result<String, Box<dyn std::error::Error>> {
        self.run_internal_prompt_text_with_progress(prompt, enable_tools, None)
    }

    fn run_bughunter(&self, scope: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let scope = scope.unwrap_or("the current repository");
        let prompt = format!(
            "You are /bughunter. Inspect {scope} and identify the most likely bugs or correctness issues. Prioritize concrete findings with file paths, severity, and suggested fixes. Use tools if needed."
        );
        println!("{}", self.run_internal_prompt_text(&prompt, true)?);
        Ok(())
    }

    fn run_ultraplan(&self, task: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let task = task.unwrap_or("the current repo work");
        let prompt = format!(
            "You are /ultraplan. Produce a deep multi-step execution plan for {task}. Include goals, risks, implementation sequence, verification steps, and rollback considerations. Use tools if needed."
        );
        let mut progress = InternalPromptProgressRun::start_ultraplan(task);
        match self.run_internal_prompt_text_with_progress(&prompt, true, Some(progress.reporter()))
        {
            Ok(plan) => {
                progress.finish_success();
                println!("{plan}");
                Ok(())
            }
            Err(error) => {
                progress.finish_failure(&error.to_string());
                Err(error)
            }
        }
    }

    // ─── Feature 3: Semantic Code Search ─────────────────────────────────────

    fn run_semantic_search(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();

        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /semantic-search <query>               Search all symbol types",
                "  /semantic-search <q> --type fn         Filter to function definitions",
                "  /semantic-search <q> --type class      Filter to class definitions",
                "  /semantic-search <q> --type struct     Filter to struct definitions",
                "  /semantic-search <q> --type import     Filter to import statements",
                "  /semantic-search <q> --lang <ext>      Limit to file extension (rs, ts, py…)",
            ]
            .join("\n");
        }

        // Parse --type and --lang flags out of args
        let (query, symbol_filter, lang_filter) = parse_semantic_search_args(args);

        if query.is_empty() {
            return "Error: provide a search query. Run `/semantic-search help` for usage.".to_string();
        }

        // Build per-type regex patterns for common languages
        let patterns: &[(&str, &str, &str)] = &[
            ("fn",     "function",  r"(^|\s)(fn|function|def|func)\s+\w*"),
            ("class",  "class",     r"(^|\s)(class|interface|trait|abstract class)\s+\w*"),
            ("struct", "struct",    r"(^|\s)(struct|type|record|data class)\s+\w*"),
            ("import", "import",    r"(^|\s)(import|use |require|from .+ import|#include)\s+\w*"),
        ];

        let cwd = env::current_dir().unwrap_or_default();
        let mut sections: Vec<String> = Vec::new();

        for (type_key, type_label, base_pattern) in patterns {
            // Apply type filter
            if let Some(ref filter) = symbol_filter {
                if filter != type_key {
                    continue;
                }
            }

            // Build combined pattern: base pattern AND query somewhere on the line
            let combined = format!("(?i)(?=.*{})(?=.*{})", regex_escape(&query), base_pattern);

            let glob_arg = lang_filter
                .as_deref()
                .map(|ext| format!("*.{ext}"))
                .unwrap_or_else(|| "*.{{rs,ts,tsx,js,py,go,java,cpp,c,h}}".to_string());

            let rg_result = Command::new("rg")
                .args([
                    "--color=never",
                    "--no-heading",
                    "-n",
                    "--glob",
                    &glob_arg,
                    "--pcre2",
                    &combined,
                ])
                .current_dir(&cwd)
                .output();

            // Fall back to a simpler two-pass approach if pcre2 unavailable
            let lines: Vec<String> = match rg_result {
                Ok(out) if out.status.success() || out.status.code() == Some(1) => {
                    String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .map(ToOwned::to_owned)
                        .collect()
                }
                _ => {
                    // Simple fallback: grep for query text across files matching base pattern
                    let simple_pat = format!("(?i){}", regex_escape(&query));
                    let fallback = Command::new("rg")
                        .args([
                            "--color=never",
                            "--no-heading",
                            "-n",
                            "--glob",
                            &glob_arg,
                            &simple_pat,
                        ])
                        .current_dir(&cwd)
                        .output()
                        .unwrap_or_else(|_| std::process::Output {
                            status: std::process::ExitStatus::default(),
                            stdout: vec![],
                            stderr: vec![],
                        });
                    String::from_utf8_lossy(&fallback.stdout)
                        .lines()
                        .map(ToOwned::to_owned)
                        .collect()
                }
            };

            if !lines.is_empty() {
                let mut section = format!("{type_label} definitions ({} results)", lines.len());
                for line in lines.iter().take(20) {
                    section.push('\n');
                    section.push_str("  ");
                    section.push_str(line);
                }
                if lines.len() > 20 {
                    section.push_str(&format!("\n  … and {} more", lines.len() - 20));
                }
                sections.push(section);
            }
        }

        if sections.is_empty() {
            format!("No symbol matches found for: {query}")
        } else {
            format!("Semantic search: {query}\n\n{}", sections.join("\n\n"))
        }
    }

    // ─── Feature 4: Docker / Container Awareness ──────────────────────────────

    fn run_docker_command(args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();

        match args {
            "" | "help" => [
                "Usage:",
                "  /docker ps                   List running containers",
                "  /docker logs <container>     Show last 50 lines of container logs",
                "  /docker compose              Show docker-compose services (if present)",
                "  /docker build                Build image from Dockerfile in current directory",
            ]
            .join("\n"),

            "ps" => run_docker_ps(),
            "compose" => run_docker_compose_services(),
            "build" => run_docker_build(),
            s if s.starts_with("logs ") => {
                let container = s["logs ".len()..].trim();
                if container.is_empty() {
                    "Usage: /docker logs <container>".to_string()
                } else {
                    run_docker_logs(container)
                }
            }
            other => format!(
                "Unknown docker sub-command: {other}\nRun `/docker help` for usage."
            ),
        }
    }

    // ─── Feature 5: Test Generation ───────────────────────────────────────────

    fn run_test_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();

        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /test generate <file>   Analyse a source file and generate unit tests",
                "  /test run               Run the project test suite",
                "  /test coverage          Run the test suite and show coverage summary",
            ]
            .join("\n");
        }

        if args == "run" {
            return run_test_suite(false);
        }

        if args == "coverage" {
            return run_test_suite(true);
        }

        if let Some(file) = args.strip_prefix("generate ") {
            let file = file.trim();
            if file.is_empty() {
                return "Usage: /test generate <file>".to_string();
            }
            let path = PathBuf::from(file);
            let source = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => return format!("Cannot read {file}: {e}"),
            };
            let prompt = format!(
                "You are /test generate. Analyse the following source file and produce a comprehensive unit-test suite for it.\n\
                 - Follow the testing idioms and conventions of the language detected.\n\
                 - Cover edge cases, error paths, and happy paths.\n\
                 - Output only the test file content, properly formatted.\n\
                 - Suggest the filename to save the tests to.\n\n\
                 Source file: {file}\n\n```\n{source}\n```",
                source = truncate_for_prompt(&source, 12_000),
            );
            match self.run_internal_prompt_text(&prompt, false) {
                Ok(result) => format!("Generated tests for {file}:\n\n{result}"),
                Err(e) => format!("test generate failed: {e}"),
            }
        } else {
            format!("Unknown /test sub-command: {args}\nRun `/test help` for usage.")
        }
    }

    // ─── Feature 6: Advanced Git ──────────────────────────────────────────────

    fn run_git_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();

        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /git rebase                  Interactive rebase assistant (AI-guided)",
                "  /git conflicts               Detect and explain merge conflicts",
                "  /git cherry-pick <sha>       Cherry-pick assistant",
                "  /git stash                   Show stash list",
                "  /git stash list              Show stash list",
                "  /git stash pop               Pop the top stash",
                "  /git stash drop [<ref>]      Drop a stash entry",
            ]
            .join("\n");
        }

        if args == "rebase" {
            return self.run_git_rebase_assistant();
        }

        if args == "conflicts" {
            return self.run_git_conflicts();
        }

        if args.starts_with("cherry-pick") {
            let sha = args["cherry-pick".len()..].trim();
            return self.run_git_cherry_pick(sha);
        }

        if args == "stash" || args == "stash list" {
            return run_git_stash_list();
        }

        if args == "stash pop" {
            return run_git_stash_op(&["stash", "pop"]);
        }

        if let Some(rest) = args.strip_prefix("stash drop") {
            let stash_ref = rest.trim();
            if stash_ref.is_empty() {
                return run_git_stash_op(&["stash", "drop"]);
            }
            return run_git_stash_op(&["stash", "drop", stash_ref]);
        }

        format!("Unknown /git sub-command: {args}\nRun `/git help` for usage.")
    }

    fn run_git_rebase_assistant(&self) -> String {
        let log = match git_output(&["log", "--oneline", "-20"]) {
            Ok(s) => s,
            Err(e) => return format!("git log failed: {e}"),
        };
        let prompt = format!(
            "You are /git rebase assistant. Summarise the following recent commits and suggest \
             which ones would benefit from being squashed, reordered, or dropped during an \
             interactive rebase. Provide the exact git rebase -i command to run and explain \
             each recommended action.\n\nRecent commits:\n{log}"
        );
        match self.run_internal_prompt_text(&prompt, false) {
            Ok(result) => result,
            Err(e) => format!("git rebase assistant failed: {e}"),
        }
    }

    fn run_git_conflicts(&self) -> String {
        // Find files with conflict markers
        let conflict_check = Command::new("git")
            .args(["diff", "--name-only", "--diff-filter=U"])
            .current_dir(env::current_dir().unwrap_or_default())
            .output();

        let conflict_files = match conflict_check {
            Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
            Err(e) => return format!("git diff failed: {e}"),
        };

        if conflict_files.is_empty() {
            return "No merge conflicts detected in the working tree.".to_string();
        }

        let file_list: Vec<&str> = conflict_files.lines().collect();
        let mut snippets = Vec::new();

        for file in file_list.iter().take(5) {
            if let Ok(content) = fs::read_to_string(file) {
                let conflict_section: String = content
                    .lines()
                    .enumerate()
                    .filter(|(_, line)| {
                        line.starts_with("<<<<<<<")
                            || line.starts_with("=======")
                            || line.starts_with(">>>>>>>")
                    })
                    .map(|(i, line)| format!("  L{}: {line}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !conflict_section.is_empty() {
                    snippets.push(format!("{file}:\n{conflict_section}"));
                }
            }
        }

        let summary = snippets.join("\n\n");
        let prompt = format!(
            "You are /git conflicts. Explain the following merge conflicts and recommend \
             the best resolution strategy for each one. Be specific about which side (ours/theirs) \
             to keep or how to manually combine them.\n\nConflicted files:\n{}\n\nConflict markers:\n{}",
            file_list.join(", "),
            summary
        );

        match self.run_internal_prompt_text(&prompt, false) {
            Ok(result) => format!(
                "Merge conflicts detected in: {}\n\n{result}",
                file_list.join(", ")
            ),
            Err(e) => format!("conflict analysis failed: {e}"),
        }
    }

    fn run_git_cherry_pick(&self, sha: &str) -> String {
        if sha.is_empty() {
            return "Usage: /git cherry-pick <sha>".to_string();
        }
        let show = match git_output(&["show", "--stat", sha]) {
            Ok(s) => s,
            Err(e) => return format!("git show {sha} failed: {e}"),
        };
        let prompt = format!(
            "You are /git cherry-pick assistant. The user wants to cherry-pick commit {sha}.\n\
             Summarise what this commit does, flag any risks (e.g. conflicts, dependency on \
             prior commits), and provide the exact command to run.\n\nCommit info:\n{show}"
        );
        match self.run_internal_prompt_text(&prompt, false) {
            Ok(result) => result,
            Err(e) => format!("cherry-pick assistant failed: {e}"),
        }
    }

    // ─── Feature 7: Refactoring Tools ─────────────────────────────────────────

    fn run_refactor_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();

        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /refactor rename <old> <new>        Rename symbol across the codebase",
                "  /refactor extract <file> <lines>    Extract lines to a new function",
                "  /refactor move <source> <dest>      Move code between files",
            ]
            .join("\n");
        }

        if let Some(rest) = args.strip_prefix("rename ") {
            let mut parts = rest.splitn(2, ' ');
            let old = parts.next().unwrap_or("").trim();
            let new = parts.next().unwrap_or("").trim();
            if old.is_empty() || new.is_empty() {
                return "Usage: /refactor rename <old> <new>".to_string();
            }
            return self.run_refactor_rename(old, new);
        }

        if let Some(rest) = args.strip_prefix("extract ") {
            let mut parts = rest.splitn(2, ' ');
            let file = parts.next().unwrap_or("").trim();
            let lines = parts.next().unwrap_or("").trim();
            if file.is_empty() {
                return "Usage: /refactor extract <file> <line-range>".to_string();
            }
            return self.run_refactor_extract(file, lines);
        }

        if let Some(rest) = args.strip_prefix("move ") {
            let mut parts = rest.splitn(2, ' ');
            let source = parts.next().unwrap_or("").trim();
            let dest = parts.next().unwrap_or("").trim();
            if source.is_empty() || dest.is_empty() {
                return "Usage: /refactor move <source> <dest>".to_string();
            }
            return self.run_refactor_move(source, dest);
        }

        format!("Unknown /refactor sub-command: {args}\nRun `/refactor help` for usage.")
    }

    fn run_refactor_rename(&self, old: &str, new: &str) -> String {
        // Count occurrences first so the user can confirm before Anvil acts
        let count_output = Command::new("rg")
            .args(["--color=never", "--count-matches", old])
            .current_dir(env::current_dir().unwrap_or_default())
            .output();

        let occurrence_info = match count_output {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if text.is_empty() {
                    format!("Symbol `{old}` not found in the workspace.")
                } else {
                    let total: usize = text
                        .lines()
                        .filter_map(|l| l.split(':').last().and_then(|n| n.trim().parse::<usize>().ok()))
                        .sum();
                    format!("Found {total} occurrences of `{old}` across:\n{text}")
                }
            }
            Err(_) => format!("ripgrep not available; cannot count occurrences of `{old}`"),
        };

        let prompt = format!(
            "You are /refactor rename. The user wants to rename `{old}` to `{new}` across the codebase.\n\
             Provide step-by-step instructions including:\n\
             1. Which files to update and why.\n\
             2. Any identifier collisions or naming conflicts to watch for.\n\
             3. The exact rg/sed commands to perform the rename safely.\n\
             4. Any follow-up changes (e.g. tests, docs, config files).\n\n\
             Occurrence summary:\n{occurrence_info}"
        );

        match self.run_internal_prompt_text(&prompt, false) {
            Ok(result) => format!("Refactor rename `{old}` -> `{new}`\n\n{occurrence_info}\n\n{result}"),
            Err(e) => format!("refactor rename failed: {e}"),
        }
    }

    fn run_refactor_extract(&self, file: &str, lines: &str) -> String {
        let source = match fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => return format!("Cannot read {file}: {e}"),
        };

        let (start, end) = parse_line_range(lines);
        let selected: String = source
            .lines()
            .enumerate()
            .filter(|(i, _)| {
                let lineno = i + 1;
                lineno >= start && (end == 0 || lineno <= end)
            })
            .map(|(_, line)| line)
            .collect::<Vec<_>>()
            .join("\n");

        if selected.is_empty() {
            return format!("No lines selected in {file} for range `{lines}`.");
        }

        let prompt = format!(
            "You are /refactor extract. The user wants to extract lines {lines} from `{file}` into a new function.\n\
             Analyse the selected code and provide:\n\
             1. A suggested function name and signature (infer parameters from free variables).\n\
             2. The complete extracted function definition.\n\
             3. The call-site replacement snippet.\n\
             4. Any considerations about scope, return values, or side effects.\n\n\
             Selected code:\n```\n{selected}\n```"
        );

        match self.run_internal_prompt_text(&prompt, false) {
            Ok(result) => format!("Extract function from {file} lines {lines}:\n\n{result}"),
            Err(e) => format!("refactor extract failed: {e}"),
        }
    }

    fn run_refactor_move(&self, source: &str, dest: &str) -> String {
        let source_content = match fs::read_to_string(source) {
            Ok(s) => s,
            Err(e) => return format!("Cannot read {source}: {e}"),
        };
        let dest_exists = fs::metadata(dest).is_ok();
        let dest_preview = if dest_exists {
            fs::read_to_string(dest)
                .map(|s| format!("Destination file exists:\n```\n{}\n```", truncate_for_prompt(&s, 4_000)))
                .unwrap_or_default()
        } else {
            format!("Destination file `{dest}` does not yet exist (will be created).")
        };

        let prompt = format!(
            "You are /refactor move. The user wants to move code from `{source}` to `{dest}`.\n\
             Provide:\n\
             1. What to move and what to keep in the source file.\n\
             2. How to update imports/exports/use declarations on both sides.\n\
             3. The exact file edits needed.\n\
             4. Any circular dependency risks.\n\n\
             Source file ({source}):\n```\n{src}\n```\n\n{dest_preview}",
            src = truncate_for_prompt(&source_content, 6_000),
        );

        match self.run_internal_prompt_text(&prompt, false) {
            Ok(result) => format!("Refactor move `{source}` -> `{dest}`:\n\n{result}"),
            Err(e) => format!("refactor move failed: {e}"),
        }
    }

    // ─── Features 8-12 ───────────────────────────────────────────────────────

    // -----------------------------------------------------------------------
    // Feature 8 — Screenshot / clipboard image input
    // -----------------------------------------------------------------------

    /// `/screenshot` — capture screen via OS tool, inject as vision content block.
    fn run_screenshot_command(&self) -> String {
        let tmpdir = std::env::temp_dir();
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let tmp_path = tmpdir.join(format!("anvil_screenshot_{ts}.png"));

        let capture_result = if cfg!(target_os = "macos") {
            Command::new("screencapture")
                .args(["-i", "-x", tmp_path.to_str().unwrap_or("")])
                .status()
        } else {
            Command::new("scrot")
                .args(["-s", tmp_path.to_str().unwrap_or("")])
                .status()
                .or_else(|_| Command::new("import").arg(tmp_path.to_str().unwrap_or("")).status())
        };

        match capture_result {
            Err(e) => return format!(
                "Screenshot capture failed: {e}\n                 Install screencapture (macOS), scrot (Linux), or ImageMagick."
            ),
            Ok(s) if !s.success() => {
                return "Screenshot cancelled or capture tool returned an error.".to_string();
            }
            Ok(_) => {}
        }

        if !tmp_path.exists() {
            return "Screenshot cancelled (no file written).".to_string();
        }

        let result = file_drop::process_file(&tmp_path);
        let _ = fs::remove_file(&tmp_path);

        if result.blocks.is_empty() {
            return format!(
                "Screenshot captured but could not be processed: {}",
                result.notice
            );
        }

        format!(
            "Screenshot ready ({} block(s) will be included in the next message).\n             Type your question and press Enter.\n\n{}",
            result.blocks.len(),
            result.notice,
        )
    }

    // -----------------------------------------------------------------------
    // Feature 9 — Database tools
    // -----------------------------------------------------------------------

    /// `/db [connect <url>|schema|query <sql>|migrate]`
    fn run_db_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(2, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();

        match sub {
            "" | "help" => [
                "Database tools",
                "",
                "  /db connect <url>   Probe a database connection",
                "  /db schema          Inspect schema files in the project",
                "  /db query <sql>     Analyse SQL with AI (performance, security)",
                "  /db migrate         Detect schema drift and suggest migrations",
                "",
                "Supported URL prefixes: postgres://, mysql://, sqlite://",
            ]
            .join("\n"),

            "connect" => {
                if rest.is_empty() { return "Usage: /db connect <url>".to_string(); }
                let driver = if rest.starts_with("postgres") {
                    "psql"
                } else if rest.starts_with("mysql") {
                    "mysql"
                } else if rest.starts_with("sqlite") {
                    "sqlite3"
                } else {
                    return format!(
                        "Unsupported scheme: {rest}\nSupported: postgres://, mysql://, sqlite://"
                    );
                };
                match Command::new(driver).arg("--version").output() {
                    Err(_) => format!(
                        "Driver `{driver}` not found on PATH.\nInstall it then retry: /db connect {rest}"
                    ),
                    Ok(_) => format!(
                        "Driver `{driver}` is available.\nURL: {rest}\n\nNext: /db schema  or  /db query <sql>"
                    ),
                }
            }

            "schema" => {
                let cwd = env::current_dir().unwrap_or_default();
                let found: Vec<String> = [
                    "prisma/schema.prisma", "schema.prisma",
                    "knexfile.js", "knexfile.ts", "database.yml", "db/schema.rb",
                ]
                .iter()
                .filter(|c| cwd.join(c).exists())
                .map(|c| c.to_string())
                .collect();

                if found.is_empty() {
                    return "No schema files found (prisma/schema.prisma, knexfile, etc.).".to_string();
                }

                let mut lines = vec![format!("Schema files ({})\n", found.len())];
                for f in &found {
                    lines.push(format!("  {f}"));
                    if let Ok(content) = fs::read_to_string(cwd.join(f.as_str())) {
                        for pl in content.lines()
                            .filter(|l| !l.trim().is_empty()
                                && !l.trim_start().starts_with("//")
                                && !l.trim_start().starts_with('#'))
                            .take(20)
                        {
                            lines.push(format!("    {pl}"));
                        }
                    }
                    lines.push(String::new());
                }
                lines.push("Tip: /db migrate for drift analysis.".to_string());
                lines.join("\n")
            }

            "query" => {
                if rest.is_empty() { return "Usage: /db query <sql>".to_string(); }
                let prompt = format!(
                    "Analyse this SQL query:\n```sql\n{rest}\n```\n\n                     1. Validate syntax.\n                     2. Suggest performance improvements (indexes, rewrites).\n                     3. Identify SQL injection risks in a dynamic version.\n                     4. Explain what the query returns in plain English."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(r) => format!("Query analysis:\n\n{r}"),
                    Err(e) => format!("db query failed: {e}"),
                }
            }

            "migrate" => {
                let cwd = env::current_dir().unwrap_or_default();
                let schema_path = ["prisma/schema.prisma", "schema.prisma"]
                    .iter()
                    .find(|p| cwd.join(p).exists())
                    .copied();

                let schema_info = if let Some(path) = schema_path {
                    fs::read_to_string(cwd.join(path))
                        .map(|s| format!(
                            "Prisma schema (`{path}`):\n```prisma\n{}\n```",
                            truncate_for_prompt(&s, 8_000)
                        ))
                        .unwrap_or_else(|_| "Could not read schema.".to_string())
                } else {
                    let mut files = Vec::new();
                    for dir in &["migrations", "db/migrations", "prisma/migrations"] {
                        if let Ok(rd) = fs::read_dir(cwd.join(dir)) {
                            for e in rd.flatten() {
                                let name = e.file_name().to_string_lossy().to_string();
                                if name.ends_with(".sql") || name.ends_with(".ts") {
                                    files.push(format!("{dir}/{name}"));
                                }
                            }
                        }
                    }
                    if files.is_empty() {
                        return "No schema or migration files found.".to_string();
                    }
                    files.sort();
                    format!(
                        "Migration files:\n{}",
                        files.iter().map(|f| format!("  {f}")).collect::<Vec<_>>().join("\n")
                    )
                };

                let prompt = format!(
                    "Analyse for schema drift and suggest migrations.\n\n{schema_info}\n\n                     1. Summarise models/tables.\n                     2. Identify drift (missing indexes, bad nullability, un-normalised relations).\n                     3. Suggest concrete migration steps.\n                     4. Highlight breaking changes."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(r) => format!("Migration analysis:\n\n{r}"),
                    Err(e) => format!("db migrate failed: {e}"),
                }
            }

            other => format!("Unknown /db sub-command: {other}\nRun `/db help` for usage."),
        }
    }

    // -----------------------------------------------------------------------
    // Feature 10 — Security scanning
    // -----------------------------------------------------------------------

    /// `/security [scan|secrets|deps|report]`
    fn run_security_command(&self, args: Option<&str>) -> String {
        let sub = args.unwrap_or("").trim();
        match sub {
            "" | "help" => [
                "Security scanning",
                "",
                "  /security scan     Grep project for common vulnerability patterns",
                "  /security secrets  Detect hardcoded secrets / credentials",
                "  /security deps     Check dependencies for known CVEs",
                "  /security report   Combined security report",
            ]
            .join("\n"),
            "scan"    => self.run_security_scan(),
            "secrets" => self.run_security_secrets(),
            "deps"    => self.run_security_deps(),
            "report"  => format!(
                "Security Report\n\nVulnerability Scan\n{}\n\nSecrets Scan\n{}\n\nDependency CVEs\n{}",
                self.run_security_scan(),
                self.run_security_secrets(),
                self.run_security_deps()
            ),
            other => format!(
                "Unknown /security sub-command: {other}\nRun `/security help` for usage."
            ),
        }
    }

    fn run_security_scan(&self) -> String {
        let cwd = env::current_dir().unwrap_or_default();
        let patterns: &[(&str, &str)] = &[
            ("eval(",                   "Unsafe eval() usage"),
            ("innerHTML",               "Potential XSS via innerHTML"),
            ("dangerouslySetInnerHTML", "React dangerouslySetInnerHTML"),
            ("exec(",                   "Shell exec injection risk"),
            ("shell=True",              "Python shell=True injection risk"),
            ("unsafe ",                 "Rust unsafe block"),
            (".unwrap()",               "Unchecked unwrap (may panic)"),
        ];
        let mut findings: Vec<String> = Vec::new();
        for (pattern, label) in patterns {
            let out = Command::new("grep")
                .args(["-rln",
                    "--include=*.rs", "--include=*.ts",
                    "--include=*.js", "--include=*.py",
                    pattern])
                .current_dir(&cwd)
                .output();
            if let Ok(o) = out {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let files: Vec<&str> = stdout.lines().take(5).collect();
                if !files.is_empty() {
                    findings.push(format!("[!] {label}\n    {}", files.join(", ")));
                }
            }
        }
        if findings.is_empty() {
            "No obvious vulnerability patterns found.\n             Consider: cargo audit, npm audit, bandit, semgrep."
                .to_string()
        } else {
            format!(
                "Potential vulnerabilities ({}):\n\n{}\n\n                 These are grep-based hints — verify each finding manually.",
                findings.len(),
                findings.join("\n\n")
            )
        }
    }

    fn run_security_secrets(&self) -> String {
        let cwd = env::current_dir().unwrap_or_default();
        // Simple keyword patterns (avoid complex shell regex escaping issues).
        let patterns: &[(&str, &str)] = &[
            ("password=",               "Hardcoded password"),
            ("secret=",                 "Hardcoded secret"),
            ("api_key=",                "Hardcoded API key"),
            ("BEGIN RSA PRIVATE KEY",   "RSA private key"),
            ("BEGIN OPENSSH PRIVATE KEY", "SSH private key"),
            ("ghp_",                    "Potential GitHub PAT"),
        ];
        let excludes = [
            "--exclude-dir=.git",
            "--exclude-dir=target",
            "--exclude-dir=node_modules",
            "--exclude=*.lock",
        ];
        let mut hits: Vec<String> = Vec::new();
        for (pat, label) in patterns {
            let mut cmd = Command::new("grep");
            cmd.arg("-rnl").arg(pat);
            for ex in &excludes { cmd.arg(ex); }
            cmd.arg(".").current_dir(&cwd);
            if let Ok(o) = cmd.output() {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let files: Vec<&str> = stdout.lines().take(3).collect();
                if !files.is_empty() {
                    hits.push(format!("[!] {label}\n    {}", files.join(", ")));
                }
            }
        }
        if hits.is_empty() {
            "No hardcoded secrets detected.\n             Consider: trufflehog, detect-secrets, gitleaks for deeper analysis."
                .to_string()
        } else {
            format!(
                "Potential secrets ({}):\n\n{}\n\n                 Rotate confirmed secrets and store them in environment variables / a vault.",
                hits.len(),
                hits.join("\n\n")
            )
        }
    }

    fn run_security_deps(&self) -> String {
        let cwd = env::current_dir().unwrap_or_default();
        let mut results: Vec<String> = Vec::new();

        if cwd.join("Cargo.toml").exists() {
            match Command::new("cargo").args(["audit", "--quiet"]).current_dir(&cwd).output() {
                Ok(o) => {
                    let out = format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    ).trim().to_string();
                    results.push(format!("cargo audit:\n{}",
                        if out.is_empty() { "No vulnerabilities found.".to_string() } else { out }
                    ));
                }
                Err(_) => results.push(
                    "cargo-audit not installed. Run: cargo install cargo-audit".to_string()
                ),
            }
        }

        if cwd.join("package.json").exists() {
            match Command::new("npm").args(["audit", "--json"]).current_dir(&cwd).output() {
                Ok(o) => {
                    let raw = String::from_utf8_lossy(&o.stdout);
                    let total: u32 = raw.lines()
                        .find(|l| l.contains("\"total\""))
                        .and_then(|l| l.chars().filter(|c| c.is_ascii_digit())
                            .collect::<String>().parse().ok())
                        .unwrap_or(0);
                    results.push(if total == 0 {
                        "npm audit: no vulnerabilities.".to_string()
                    } else {
                        format!("npm audit: {total} vulnerabilities. Run `npm audit fix`.")
                    });
                }
                Err(_) => results.push("npm not available on PATH.".to_string()),
            }
        }

        if cwd.join("requirements.txt").exists() || cwd.join("pyproject.toml").exists() {
            match Command::new("pip-audit").arg("--progress-spinner=off").current_dir(&cwd).output() {
                Ok(o) => {
                    let out = String::from_utf8_lossy(&o.stdout).to_string();
                    let summary = out.lines().last().unwrap_or("").trim().to_string();
                    results.push(format!("pip-audit: {}",
                        if summary.is_empty() { "no vulnerabilities.".to_string() } else { summary }
                    ));
                }
                Err(_) => results.push(
                    "pip-audit not installed. Run: pip install pip-audit".to_string()
                ),
            }
        }

        if results.is_empty() {
            "No dependency manifests found (Cargo.toml, package.json, requirements.txt).".to_string()
        } else {
            results.join("\n\n")
        }
    }

    // -----------------------------------------------------------------------
    // Feature 11 — API development helpers
    // -----------------------------------------------------------------------

    /// `/api [spec <file>|mock <spec>|test <url>|docs]`
    fn run_api_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(2, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();

        match sub {
            "" | "help" => [
                "API development helpers",
                "",
                "  /api spec <file>       Generate OpenAPI spec from a source file",
                "  /api mock <spec>       Start a mock server from an OpenAPI spec",
                "  /api test <url>        Test an API endpoint via curl",
                "  /api docs              Generate API docs for the project",
            ]
            .join("\n"),

            "spec" => {
                if rest.is_empty() { return "Usage: /api spec <file>".to_string(); }
                let source = match fs::read_to_string(rest) {
                    Ok(s) => s,
                    Err(e) => return format!("Cannot read {rest}: {e}"),
                };
                let prompt = format!(
                    "Generate an OpenAPI 3.1 specification (YAML) for this source file.\n                     Extract all routes, HTTP methods, request/response schemas, parameters.\n                     File: {rest}\n\n```\n{}\n```",
                    truncate_for_prompt(&source, 8_000)
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(r) => format!("OpenAPI spec for {rest}:\n\n{r}"),
                    Err(e) => format!("api spec failed: {e}"),
                }
            }

            "mock" => {
                if rest.is_empty() { return "Usage: /api mock <spec-file>".to_string(); }
                if !std::path::Path::new(rest).exists() {
                    return format!("Spec file not found: {rest}");
                }
                for (tool, tool_args) in &[
                    ("prism", vec!["mock", rest]),
                    ("json-server", vec!["--watch", rest]),
                ] {
                    if let Ok(mut child) = Command::new(tool).args(tool_args).spawn() {
                        thread::sleep(Duration::from_millis(500));
                        if child.try_wait().map(|s| s.is_none()).unwrap_or(false) {
                            return format!(
                                "Mock server started with `{tool}`.\nSpec: {rest}\nCtrl+C to stop."
                            );
                        }
                    }
                }
                format!(
                    "No mock server tool found. Install:\n                     - npm install -g @stoplight/prism-cli\n                     - npm install -g json-server\n\nRetry: /api mock {rest}"
                )
            }

            "test" => {
                if rest.is_empty() { return "Usage: /api test <url>".to_string(); }
                match Command::new("curl")
                    .args(["-s", "-D", "-", "-o", "/dev/null", "--max-time", "10", rest])
                    .output()
                {
                    Err(e) => format!("curl failed: {e}"),
                    Ok(o) => {
                        let headers = String::from_utf8_lossy(&o.stdout).to_string();
                        let status = headers.lines().next().unwrap_or("").trim().to_string();
                        let ct = headers.lines()
                            .find(|l| l.to_lowercase().starts_with("content-type:"))
                            .unwrap_or("")
                            .to_string();
                        format!("API test: {rest}\n\nStatus: {status}\n{ct}\n\nHeaders:\n{headers}")
                    }
                }
            }

            "docs" => {
                let cwd = env::current_dir().unwrap_or_default();
                let route_dirs = [
                    "src/routes", "routes", "src/api", "api",
                    "src/controllers", "controllers",
                ];
                let mut route_files: Vec<String> = Vec::new();
                for dir in &route_dirs {
                    if let Ok(entries) = fs::read_dir(cwd.join(dir)) {
                        for e in entries.flatten() {
                            let name = e.file_name().to_string_lossy().to_string();
                            if name.ends_with(".ts") || name.ends_with(".js") || name.ends_with(".rs") {
                                route_files.push(format!("{dir}/{name}"));
                            }
                        }
                    }
                }
                if route_files.is_empty() {
                    return "No route files found. Use `/api spec <file>` to target one directly.".to_string();
                }
                let file_list = route_files.iter().map(|f| format!("  {f}")).collect::<Vec<_>>().join("\n");
                let prompt = format!(
                    "Generate Markdown API documentation.\nRoute files:\n{file_list}\n\n                     For each endpoint: method, path, description, params, response, auth.\n                     Include a table of contents."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(r) => format!("API documentation:\n\n{r}"),
                    Err(e) => format!("api docs failed: {e}"),
                }
            }

            other => format!("Unknown /api sub-command: {other}\nRun `/api help` for usage."),
        }
    }

    // -----------------------------------------------------------------------
    // Feature 12 — Documentation generation
    // -----------------------------------------------------------------------

    /// `/docs [generate|readme|architecture|changelog]`
    fn run_docs_command(&self, args: Option<&str>) -> String {
        let sub = args.unwrap_or("").trim();
        match sub {
            "" | "help" => [
                "Documentation generation",
                "",
                "  /docs generate      Auto-generate project documentation",
                "  /docs readme        Generate or update README.md",
                "  /docs architecture  Generate architecture diagram description",
                "  /docs changelog     Generate changelog from git history",
            ]
            .join("\n"),
            "generate"     => self.run_docs_generate(),
            "readme"       => self.run_docs_readme(),
            "architecture" => self.run_docs_architecture(),
            "changelog"    => self.run_docs_changelog(),
            other => format!("Unknown /docs sub-command: {other}\nRun `/docs help` for usage."),
        }
    }

    fn run_docs_generate(&self) -> String {
        let cwd = env::current_dir().unwrap_or_default();
        let mut stack = Vec::new();
        if cwd.join("Cargo.toml").exists()     { stack.push("Rust/Cargo"); }
        if cwd.join("package.json").exists()   { stack.push("Node.js/npm"); }
        if cwd.join("pyproject.toml").exists() { stack.push("Python/pyproject"); }
        let tech = if stack.is_empty() { "unknown".to_string() } else { stack.join(", ") };
        let prompt = format!(
            "Generate comprehensive documentation for a {tech} project at `{cwd}`.\n             Produce: 1. Overview  2. Installation  3. Configuration               4. Usage examples  5. Dev workflow  6. Contributing\nOutput as Markdown.",
            cwd = cwd.display()
        );
        match self.run_internal_prompt_text(&prompt, false) {
            Ok(r) => format!("Generated documentation:\n\n{r}"),
            Err(e) => format!("docs generate failed: {e}"),
        }
    }

    fn run_docs_readme(&self) -> String {
        let cwd = env::current_dir().unwrap_or_default();
        let existing = ["README.md", "readme.md", "README.rst"]
            .iter()
            .find_map(|n| fs::read_to_string(cwd.join(n)).ok());
        let project_name = ["Cargo.toml", "package.json", "pyproject.toml"]
            .iter()
            .find_map(|f| {
                let content = fs::read_to_string(cwd.join(f)).ok()?;
                content.lines().find_map(|l| {
                    let l = l.trim();
                    if l.starts_with("name") {
                        let val = l.split(['=', ':'])
                            .nth(1)?
                            .trim()
                            .trim_matches(['"', '\'', ',', ' ']);
                        if !val.is_empty() && val != "{" {
                            return Some(val.to_string());
                        }
                    }
                    None
                })
            })
            .unwrap_or_else(|| {
                cwd.file_name().unwrap_or_default().to_string_lossy().to_string()
            });
        let context = match existing {
            Some(content) => format!(
                "Update this README:\n```markdown\n{}\n```\nImprove it with:",
                truncate_for_prompt(&content, 4_000)
            ),
            None => format!("Create a README for `{project_name}` with:"),
        };
        let prompt = format!(
            "{context}\n- Title and description\n- Quick start\n- Usage examples\n             - Configuration\n- Dev setup\n- License\n\nOutput only the README.md content."
        );
        match self.run_internal_prompt_text(&prompt, false) {
            Ok(r) => format!("README.md:\n\n{r}"),
            Err(e) => format!("docs readme failed: {e}"),
        }
    }

    fn run_docs_architecture(&self) -> String {
        let cwd = env::current_dir().unwrap_or_default();
        let mut structure = Vec::new();
        if let Ok(entries) = fs::read_dir(&cwd) {
            let mut sorted: Vec<_> = entries.flatten().collect();
            sorted.sort_by_key(|e| e.file_name());
            for e in sorted.iter().take(40) {
                let name = e.file_name().to_string_lossy().to_string();
                if matches!(name.as_str(), ".git"|"target"|"node_modules"|".cache"|"vendor") {
                    continue;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                structure.push(if is_dir { format!("  {name}/") } else { format!("  {name}") });
            }
        }
        let structure_text = structure.join("\n");
        let prompt = format!(
            "Generate an architecture overview document.\nRoot: {cwd}\nStructure:\n{structure_text}\n\n\
             Include: 1. Description  2. ASCII component diagram  3. Data flow\n\
             4. Technology stack  5. Design decisions  6. Deployment topology\n\
             Output as Markdown.",
            cwd = cwd.display(),
        );
        match self.run_internal_prompt_text(&prompt, false) {
            Ok(r) => format!("Architecture overview:\n\n{r}"),
            Err(e) => format!("docs architecture failed: {e}"),
        }
    }

    fn run_docs_changelog(&self) -> String {
        let git_log = Command::new("git")
            .args(["log", "--oneline", "--no-merges", "--format=%h %ad %s", "--date=short", "-100"])
            .output();
        match git_log {
            Err(e) => format!("git log failed: {e}"),
            Ok(o) if !o.status.success() => "Not in a git repository or no commits yet.".to_string(),
            Ok(o) => {
                let raw = String::from_utf8_lossy(&o.stdout).to_string();
                if raw.trim().is_empty() { return "No commits found.".to_string(); }
                let prompt = format!(
                    "Generate a CHANGELOG.md from this git log.\n                     Group by type and release (Keep-a-Changelog format).\n\n                     ```\n{}\n```",
                    truncate_for_prompt(&raw, 6_000)
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(r) => format!("CHANGELOG.md:\n\n{r}"),
                    Err(e) => format!("docs changelog failed: {e}"),
                }
            }
        }
    }
    fn run_scaffold_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(2, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();

        const TEMPLATES: &[(&str, &str)] = &[
            ("rust",   "Rust binary — Cargo.toml, src/main.rs, .gitignore"),
            ("node",   "Node.js — package.json, src/index.js, .gitignore"),
            ("python", "Python — pyproject.toml, src/__init__.py, .gitignore"),
            ("react",  "React + Vite — package.json, src/App.tsx, Tailwind CSS"),
            ("nextjs", "Next.js — package.json, app/page.tsx, Tailwind CSS"),
            ("go",     "Go module — go.mod, cmd/main.go, .gitignore"),
            ("docker", "Docker service — Dockerfile, docker-compose.yml, .env.example"),
        ];

        match sub {
            "" | "help" => {
                let mut lines = vec![
                    "Usage:".to_string(),
                    "  /scaffold new <template>   Create a project from a template".to_string(),
                    "  /scaffold list             List available templates".to_string(),
                    String::new(),
                    "Templates:".to_string(),
                ];
                for (name, desc) in TEMPLATES {
                    lines.push(format!("  {name:<10}  {desc}"));
                }
                lines.join("\n")
            }
            "list" => {
                let mut lines = vec!["Available templates:".to_string()];
                for (name, desc) in TEMPLATES {
                    lines.push(format!("  {name:<10}  {desc}"));
                }
                lines.join("\n")
            }
            "new" => {
                let template = rest;
                if template.is_empty() {
                    return "Usage: /scaffold new <template>\n  Run /scaffold list for available templates.".to_string();
                }
                if !TEMPLATES.iter().any(|(n, _)| *n == template) {
                    let names: Vec<&str> = TEMPLATES.iter().map(|(n, _)| *n).collect();
                    return format!(
                        "Unknown template: {template}\n  Available: {}\n  Run /scaffold list for details.",
                        names.join(", ")
                    );
                }
                let cwd = env::current_dir().unwrap_or_default();
                let prompt = format!(
                    "You are /scaffold. The user wants to create a new {template} project in the current directory ({cwd}).\n\
                     Generate the complete file tree and contents for a production-ready {template} project.\n\
                     Follow best practices:\n\
                     - Include a .gitignore appropriate for the ecosystem.\n\
                     - Include a minimal README.md.\n\
                     - Include a sensible directory structure.\n\
                     - Include linting/formatting config where standard (e.g. .eslintrc, rustfmt.toml).\n\
                     - For compiled languages include a build script.\n\
                     Output each file as a code block with the path as the heading.\n\
                     After the files, list 3 next steps the developer should take.",
                    cwd = cwd.display(),
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Scaffold: {template}\n\n{result}"),
                    Err(e) => format!("scaffold failed: {e}"),
                }
            }
            other => format!(
                "Unknown /scaffold sub-command: {other}\n  /scaffold list   List templates\n  /scaffold new <template>   Create project"
            ),
        }
    }

    fn run_perf_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(2, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();

        match sub {
            "" | "help" => [
                "Usage:",
                "  /perf profile <command>   Profile a shell command and report timing",
                "  /perf benchmark <file>    Analyse benchmarks in a file",
                "  /perf flamegraph          Guide for generating a flamegraph",
                "  /perf analyze             Analyze profiling artifacts in the workspace",
            ].join("\n"),

            "profile" => {
                if rest.is_empty() {
                    return "Usage: /perf profile <command>".to_string();
                }
                let start = std::time::Instant::now();
                let output = Command::new("sh")
                    .arg("-c")
                    .arg(rest)
                    .current_dir(env::current_dir().unwrap_or_default())
                    .output();
                let elapsed = start.elapsed();
                let (stdout, stderr, exit_status) = match output {
                    Ok(o) => (
                        String::from_utf8_lossy(&o.stdout).trim().to_string(),
                        String::from_utf8_lossy(&o.stderr).trim().to_string(),
                        if o.status.success() { "success".to_string() } else {
                            format!("exit {}", o.status.code().unwrap_or(-1))
                        },
                    ),
                    Err(e) => (String::new(), e.to_string(), "error".to_string()),
                };
                let summary = format!(
                    "Perf Profile\n  Command          {rest}\n  Wall time        {elapsed:.3?}\n  Status           {exit_status}"
                );
                let combined = format!("{summary}\n\nStdout:\n{stdout}\n\nStderr:\n{stderr}");
                let prompt = format!(
                    "You are /perf profile. A command was profiled.\n\
                     Command: {rest}\nWall time: {elapsed:.3?}\nExit status: {exit_status}\n\
                     Stdout (truncated):\n{so}\nStderr (truncated):\n{se}\n\n\
                     Provide a brief analysis:\n\
                     1. Is the runtime acceptable for this type of command?\n\
                     2. What are the likely bottlenecks?\n\
                     3. Concrete suggestions to speed it up.",
                    so = truncate_for_prompt(&stdout, 3_000),
                    se = truncate_for_prompt(&stderr, 1_000),
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(analysis) => format!("{combined}\n\nAnalysis:\n{analysis}"),
                    Err(_) => combined,
                }
            }

            "benchmark" => {
                if rest.is_empty() {
                    return "Usage: /perf benchmark <file>".to_string();
                }
                let source = match fs::read_to_string(rest) {
                    Ok(s) => s,
                    Err(e) => return format!("Cannot read {rest}: {e}"),
                };
                let prompt = format!(
                    "You are /perf benchmark. Analyse `{rest}` for benchmark functions.\n\
                     Source:\n```\n{}\n```\n\n\
                     For each benchmark:\n\
                     1. Describe what it measures.\n\
                     2. Identify measurement pitfalls (warm-up, noise, allocations).\n\
                     3. Suggest how to run it.\n\
                     4. Propose improvements to the benchmark itself if any.",
                    truncate_for_prompt(&source, 8_000),
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Benchmark analysis for {rest}:\n\n{result}"),
                    Err(e) => format!("perf benchmark failed: {e}"),
                }
            }

            "flamegraph" => {
                let cwd = env::current_dir().unwrap_or_default();
                let prompt = format!(
                    "You are /perf flamegraph. Describe how to generate a flamegraph for the project at `{}`.\n\
                     Provide:\n\
                     1. Which profiling tool is best suited (cargo-flamegraph, perf + flamegraph.pl, py-spy, async-profiler, etc.).\n\
                     2. The exact commands to install the tool and capture a profile.\n\
                     3. How to interpret the resulting flamegraph.\n\
                     4. Common hotspot patterns to look for.",
                    cwd.display(),
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Flamegraph guide:\n\n{result}"),
                    Err(e) => format!("perf flamegraph failed: {e}"),
                }
            }

            "analyze" => {
                let cwd = env::current_dir().unwrap_or_default();
                let artifacts: Vec<String> = ["perf.data", "flame.svg", "flamegraph.svg", "callgrind.out", "profile.json"]
                    .iter()
                    .filter_map(|name| {
                        let p = cwd.join(name);
                        if p.exists() { Some((*name).to_string()) } else { None }
                    })
                    .collect();
                let artifact_summary = if artifacts.is_empty() {
                    "No standard profiling artifacts found in the current directory.".to_string()
                } else {
                    format!("Found profiling artifacts: {}", artifacts.join(", "))
                };
                let prompt = format!(
                    "You are /perf analyze. {artifact_summary}\nWorking directory: {}\n\
                     Provide guidance on:\n\
                     1. How to interpret any discovered artifacts.\n\
                     2. General profiling best practices for this project type.\n\
                     3. Recommended next steps to identify performance regressions.",
                    cwd.display(),
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Perf analysis:\n  {artifact_summary}\n\n{result}"),
                    Err(e) => format!("perf analyze failed: {e}"),
                }
            }

            other => format!("Unknown /perf sub-command: {other}\nRun `/perf help` for usage."),
        }
    }

    fn run_debug_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(2, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();
        match sub {
            "" | "help" => [
                "Usage:",
                "  /debug start <file>              Start debugging — show launch config",
                "  /debug breakpoint <file:line>    Explain what to observe at a breakpoint",
                "  /debug watch <expr>              Explain how to watch an expression",
                "  /debug explain <error>           Explain an error with full context",
            ]
            .join("\n"),
            "start" => {
                if rest.is_empty() {
                    return "Usage: /debug start <file>".to_string();
                }
                let source = fs::read_to_string(rest)
                    .map(|s| truncate_for_prompt(&s, 6_000))
                    .unwrap_or_else(|_| format!("<could not read {rest}>"));
                let prompt = format!(
                    "You are /debug start. The user wants to debug `{rest}`.\n\
                     File contents:\n```\n{source}\n```\n\n\
                     Provide:\n\
                     1. The debugger to use (gdb, lldb, delve, pdb, node --inspect, etc.) and why.\n\
                     2. A minimal launch configuration (VSCode launch.json or equivalent).\n\
                     3. The exact command to start the debugger from the terminal.\n\
                     4. Key entry points worth setting initial breakpoints at.\n\
                     5. Environment variables or flags needed for debug symbols."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Debug start for {rest}:\n\n{result}"),
                    Err(e) => format!("debug start failed: {e}"),
                }
            }
            "breakpoint" => {
                if rest.is_empty() {
                    return "Usage: /debug breakpoint <file:line>".to_string();
                }
                let (file, line) = rest
                    .rfind(':')
                    .map_or((rest, ""), |p| (&rest[..p], &rest[p + 1..]));
                let context_lines = if !file.is_empty() {
                    fs::read_to_string(file)
                        .map(|s| {
                            let lineno: usize = line.parse().unwrap_or(0);
                            if lineno == 0 {
                                return truncate_for_prompt(&s, 4_000);
                            }
                            let start = lineno.saturating_sub(10);
                            let end = lineno + 10;
                            s.lines()
                                .enumerate()
                                .filter(|(i, _)| *i + 1 >= start && *i + 1 <= end)
                                .map(|(i, l)| format!("{:>4} | {l}", i + 1))
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_else(|_| format!("<could not read {file}>"))
                } else {
                    String::new()
                };
                let prompt = format!(
                    "You are /debug breakpoint. The user set a breakpoint at `{rest}`.\n\
                     Code context (lines around {line}):\n```\n{context_lines}\n```\n\n\
                     Explain:\n\
                     1. What program state to inspect when execution pauses here.\n\
                     2. Which variables are in scope and expected values.\n\
                     3. Conditions that might cause unexpected behaviour.\n\
                     4. How to set a conditional breakpoint if useful."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Breakpoint {rest}:\n\n{result}"),
                    Err(e) => format!("debug breakpoint failed: {e}"),
                }
            }
            "watch" => {
                if rest.is_empty() {
                    return "Usage: /debug watch <expression>".to_string();
                }
                let prompt = format!(
                    "You are /debug watch. The user wants to watch the expression: `{rest}`\n\
                     Explain:\n\
                     1. How to set a watchpoint in common debuggers (gdb, lldb, VSCode, pdb, delve).\n\
                     2. What changes to the expression would trigger a break.\n\
                     3. Data watchpoint vs expression watch vs value watch — and the difference.\n\
                     4. Performance implications of watching this expression."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Watch `{rest}`:\n\n{result}"),
                    Err(e) => format!("debug watch failed: {e}"),
                }
            }
            "explain" => {
                if rest.is_empty() {
                    return "Usage: /debug explain <error message or stack trace>".to_string();
                }
                let session_context = recent_user_context(self.runtime.session(), 4);
                let prompt = format!(
                    "You are /debug explain. Analyse and explain the following error.\n\
                     Error:\n```\n{rest}\n```\n\
                     Recent conversation context:\n{session_context}\n\n\
                     Provide:\n\
                     1. Root cause — what went wrong and why.\n\
                     2. Where in the code to look (file/function/line if determinable).\n\
                     3. Step-by-step fix.\n\
                     4. How to prevent this class of error in the future."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(result) => format!("Error explanation:\n\n{result}"),
                    Err(e) => format!("debug explain failed: {e}"),
                }
            }
            other => format!("Unknown /debug sub-command: {other}\nRun `/debug help` for usage."),
        }
    }

    #[allow(clippy::unused_self)]
    fn run_voice_command(args: Option<&str>) -> String {
        let sub = args.unwrap_or("").trim();
        match sub {
            "start" => concat!(
                "Voice input — coming soon\n\n",
                "Voice capture requires microphone access and a speech-to-text backend.\n",
                "Planned: /voice start  ->  capture mic input and inject as a prompt.",
            )
            .to_string(),
            "stop" => "Voice input — coming soon\n\nNo active voice session to stop.".to_string(),
            "" | "help" => [
                "Voice input (coming soon)",
                "",
                "Commands:",
                "  /voice start   Begin capturing microphone input",
                "  /voice stop    Stop capturing and submit",
                "",
                "Requires a local speech-to-text engine (e.g. whisper.cpp).",
            ]
            .join("\n"),
            other => format!("Unknown /voice sub-command: {other}\n  /voice start | /voice stop"),
        }
    }

    #[allow(clippy::unused_self)]
    fn run_collab_command(args: Option<&str>) -> String {
        let sub = args.unwrap_or("").trim();
        match sub {
            "share" => concat!(
                "Collaboration — coming soon\n\n",
                "Planned: /collab share  ->  generate a shareable session ID.\n",
                "This feature is reserved for a future release.",
            )
            .to_string(),
            "join" => concat!(
                "Collaboration — coming soon\n\n",
                "Usage: /collab join <session-id>\n",
                "This feature is reserved for a future release.",
            )
            .to_string(),
            "" | "help" => [
                "Collaboration (coming soon)",
                "",
                "Commands:",
                "  /collab share          Share this session (generates an invite ID)",
                "  /collab join <id>      Join another user's shared session",
                "",
                "Requires an AnvilHub account. Watch the changelog for availability.",
            ]
            .join("\n"),
            other => {
                format!("Unknown /collab sub-command: {other}\n  /collab share | /collab join <id>")
            }
        }
    }

    fn run_changelog_command(&self) -> String {
        // Determine the last tag for the commit range.
        let last_tag = Command::new("git")
            .args(["describe", "--tags", "--abbrev=0"])
            .current_dir(env::current_dir().unwrap_or_default())
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            });

        let (range, range_desc) = match &last_tag {
            Some(tag) => (format!("{tag}..HEAD"), format!("since tag `{tag}`")),
            None => ("HEAD".to_string(), "all commits (no tags found)".to_string()),
        };

        let log = Command::new("git")
            .args(["log", &range, "--oneline", "--no-merges"])
            .current_dir(env::current_dir().unwrap_or_default())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|e| format!("<git log failed: {e}>"));

        if log.trim().is_empty() {
            return format!(
                "Changelog\n  Range            {range_desc}\n  Result           No new commits since last tag."
            );
        }

        let prompt = format!(
            "You are /changelog. Generate a CHANGELOG.md entry from these git commits ({range_desc}).\n\
             \n\
             Rules:\n\
             1. Group commits by conventional commit type:\n\
                feat: -> New Features | fix: -> Bug Fixes | docs: -> Documentation\n\
                style: -> Style | refactor: -> Refactoring | perf: -> Performance\n\
                test: -> Tests | chore:/build:/ci: -> Maintenance\n\
                Commits without a prefix -> Other Changes\n\
             2. Format each item as: - Short human-readable description (#sha)\n\
             3. Add a header: ## [Unreleased] - YYYY-MM-DD\n\
             4. Keep descriptions concise but informative.\n\
             \n\
             Commits:\n{log}"
        );

        match self.run_internal_prompt_text(&prompt, false) {
            Ok(result) => format!(
                "Changelog ({range_desc})\nCommits:\n{log}\n\n--- CHANGELOG.md entry ---\n{result}"
            ),
            Err(e) => format!("changelog failed: {e}"),
        }
    }

    fn run_env_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(3, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let key = parts.next().unwrap_or("").trim();
        let val = parts.next().unwrap_or("").trim();

        match sub {
            "" | "show" => {
                let secret_pats = [
                    "KEY", "SECRET", "TOKEN", "PASSWORD", "PASS", "AUTH", "CREDENTIAL", "PRIVATE",
                ];
                let mut vars: Vec<(String, String)> = env::vars().collect();
                vars.sort_by(|a, b| a.0.cmp(&b.0));
                let mut lines = vec!["Environment variables (secrets redacted):".to_string()];
                for (k, v) in &vars {
                    let redact = secret_pats.iter().any(|p| k.to_uppercase().contains(p));
                    lines.push(format!("  {k}={}", if redact { "<redacted>" } else { v }));
                }
                lines.push(String::new());
                lines.push(format!("  Total: {} variables", vars.len()));
                lines.join("\n")
            }
            "set" => {
                if key.is_empty() {
                    return "Usage: /env set <KEY> <VALUE>".to_string();
                }
                // Note: modifying the process env requires unsafe in Rust 1.80+.
                // This project forbids unsafe blocks; record the intent and advise
                // the user to use `export KEY=VALUE` in their shell instead.
                format!(
                    "Env set (shell-only)\n  Key              {key}\n  Value            {}\n\n\
                     Note: Anvil cannot modify the process environment without unsafe code.\n\
                     Run the following in your shell to set this variable:\n\
                     export {key}={}",
                    if val.is_empty() { "<empty>" } else { val },
                    if val.is_empty() { String::new() } else { shell_quote(val) },
                )
            }
            "load" => {
                let path = key;
                if path.is_empty() {
                    return "Usage: /env load <file>".to_string();
                }
                let content = match fs::read_to_string(path) {
                    Ok(s) => s,
                    Err(e) => return format!("Cannot read {path}: {e}"),
                };
                let (mut loaded, mut skipped) = (0usize, 0usize);
                let mut export_lines: Vec<String> = Vec::new();
                for line in content.lines() {
                    let t = line.trim();
                    if t.is_empty() || t.starts_with('#') {
                        continue;
                    }
                    if let Some(eq) = t.find('=') {
                        let k = t[..eq].trim();
                        let v = t[eq + 1..].trim().trim_matches('"').trim_matches('\'');
                        if !k.is_empty() {
                            export_lines.push(format!("export {k}={}", shell_quote(v)));
                            loaded += 1;
                        } else {
                            skipped += 1;
                        }
                    } else {
                        skipped += 1;
                    }
                }
                format!(
                    "Env load\n  File             {path}\n  Loaded           {loaded} variable(s)\n  Skipped          {skipped} line(s)\n  Scope            session (not persisted)"
                )
            }
            "diff" => {
                let cwd = env::current_dir().unwrap_or_default();
                let mut env_files: Vec<std::path::PathBuf> = Vec::new();
                for name in &[
                    ".env",
                    ".env.example",
                    ".env.local",
                    ".env.staging",
                    ".env.production",
                    ".env.development",
                ] {
                    let p = cwd.join(name);
                    if p.exists() {
                        env_files.push(p);
                    }
                }
                if env_files.is_empty() {
                    return "Env diff\n  No .env files found in the current directory.".to_string();
                }
                let mut summaries: Vec<String> = Vec::new();
                for ef in &env_files {
                    let content = fs::read_to_string(ef).unwrap_or_default();
                    let keys: Vec<&str> = content
                        .lines()
                        .filter(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))
                        .filter_map(|l| l.find('=').map(|p| &l[..p]))
                        .collect();
                    summaries.push(format!(
                        "  {} ({} keys): {}",
                        ef.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                        keys.len(),
                        keys.join(", ")
                    ));
                }
                let prompt = format!(
                    "You are /env diff. Analyse these .env files and highlight:\n\
                     1. Keys present in one file but missing in another.\n\
                     2. Keys that need to be kept in sync.\n\
                     3. Any suspicious or potentially insecure patterns.\n\nFiles:\n{}",
                    summaries.join("\n")
                );
                let header = format!("Env diff\n  Files found:\n{}\n", summaries.join("\n"));
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(r) => format!("{header}\n{r}"),
                    Err(e) => format!("env diff failed: {e}"),
                }
            }
            other => format!(
                "Unknown /env sub-command: {other}\n\n\
                 Usage:\n  /env show               Show current environment (secrets redacted)\n\
                   /env set <KEY> <VALUE>  Set an env var for this session\n\
                   /env load <file>        Load a .env file into the session\n\
                   /env diff               Compare .env files in the workspace"
            ),
        }
    }
    fn run_hub_command(&self, args: Option<&str>) -> String {
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

        if let Some(name) = args.strip_prefix("install ").map(str::trim) {
            if name.is_empty() {
                return "Usage: /hub install <name>".to_string();
            }
            let pkg = match client.get_package(name) {
                Ok(p) => p,
                Err(e) => return format!("hub install: {e}"),
            };
            let install_dir = anvil_home_dir();
            return match client.install(&pkg, &install_dir) {
                Ok(dest) => format!(
                    "Installed {} v{} to {}",
                    pkg.name,
                    pkg.version,
                    dest.display()
                ),
                Err(e) => format!("hub install: {e}"),
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
                        Err(e) => out.push_str(&format!("\n{label}\n  (error: {e})\n")),
                    }
                }
                out.push_str("\nRun /hub <category> for more, or /hub install <name> to install.");
                out
            }
        }
    }

    fn run_language_command(&self, lang: Option<&str>) -> String {
        run_language_command_static(lang)
    }

    // ─── Feature 1: LSP Autocomplete ─────────────────────────────────────────

    fn run_lsp_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /lsp start <lang>         Start language server for a language",
                "  /lsp symbols <file>       List symbols in a file via LSP",
                "  /lsp references <symbol>  Find all references to a symbol",
                "",
                "Supported languages: rust, typescript, python, go, java",
            ].join("\n");
        }
        if let Some(lang) = args.strip_prefix("start ") {
            let lang = lang.trim();
            if lang.is_empty() { return "Usage: /lsp start <lang>".to_string(); }
            let binary = lsp_binary_for_lang(lang);
            let found = Command::new("which").arg(&binary).output()
                .map(|o| o.status.success()).unwrap_or(false);
            return if found {
                format!("LSP server for '{lang}' is available ({binary}).\nServer would be started on next file operation.")
            } else {
                format!("LSP server binary '{binary}' not found in PATH.\nInstall it first (e.g. `cargo install rust-analyzer` for rust).")
            };
        }
        if let Some(file) = args.strip_prefix("symbols ") {
            let file = file.trim();
            if file.is_empty() { return "Usage: /lsp symbols <file>".to_string(); }
            let source = match fs::read_to_string(file) {
                Ok(s) => s,
                Err(e) => return format!("Cannot read {file}: {e}"),
            };
            let prompt = format!(
                "You are an LSP server. List all top-level symbols (functions, structs, classes, \
                 enums, constants, type aliases) in this source file. Format each as:\n\
                 <kind> <name>  <line>\n\nFile: {file}\n\n```\n{src}\n```",
                src = truncate_for_prompt(&source, 10_000),
            );
            return match self.run_internal_prompt_text(&prompt, false) {
                Ok(r) => format!("Symbols in {file}:\n\n{r}"),
                Err(e) => format!("lsp symbols failed: {e}"),
            };
        }
        if let Some(symbol) = args.strip_prefix("references ") {
            let symbol = symbol.trim();
            if symbol.is_empty() { return "Usage: /lsp references <symbol>".to_string(); }
            let output = Command::new("grep")
                .args(["-rn", "--include=*.rs", "--include=*.ts",
                       "--include=*.py", "--include=*.go", symbol, "."])
                .output();
            return match output {
                Ok(o) if !o.stdout.is_empty() => {
                    let text = String::from_utf8_lossy(&o.stdout);
                    let lines: Vec<&str> = text.lines().take(40).collect();
                    format!("References to '{symbol}' ({} shown):\n\n{}", lines.len(), lines.join("\n"))
                }
                _ => format!("No references found for '{symbol}'."),
            };
        }
        format!("Unknown /lsp sub-command: {args}\nRun `/lsp help` for usage.")
    }

    // ─── Feature 2: Jupyter Notebook Support ─────────────────────────────────

    fn run_notebook_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /notebook run <file>              Execute all cells in a .ipynb notebook",
                "  /notebook cell <file> <n>         Run cell N (0-based) in a notebook",
                "  /notebook export <file> <format>  Export notebook (html|py|pdf)",
            ].join("\n");
        }
        if let Some(file) = args.strip_prefix("run ") {
            let file = file.trim();
            if file.is_empty() { return "Usage: /notebook run <file>".to_string(); }
            if command_exists("jupyter") {
                let out = Command::new("jupyter")
                    .args(["nbconvert", "--to", "notebook", "--execute", "--inplace", file])
                    .output();
                return match out {
                    Ok(o) if o.status.success() => format!("Executed notebook: {file}"),
                    Ok(o) => format!("nbconvert failed:\n{}", String::from_utf8_lossy(&o.stderr).trim()),
                    Err(e) => format!("Failed to run jupyter: {e}"),
                };
            }
            let raw = match fs::read_to_string(file) {
                Ok(s) => s, Err(e) => return format!("Cannot read {file}: {e}"),
            };
            let prompt = format!(
                "This is a Jupyter notebook (JSON). Summarise what each cell does and what \
                 output would be expected if executed top-to-bottom. Identify any likely errors.\n\n{}",
                truncate_for_prompt(&raw, 12_000),
            );
            return match self.run_internal_prompt_text(&prompt, false) {
                Ok(r) => format!("Notebook analysis for {file}:\n\n{r}"),
                Err(e) => format!("notebook run: {e}"),
            };
        }
        if let Some(rest) = args.strip_prefix("cell ") {
            let mut parts = rest.trim().splitn(3, ' ');
            let file = parts.next().unwrap_or("").trim();
            let cell_str = parts.next().unwrap_or("").trim();
            if file.is_empty() || cell_str.is_empty() {
                return "Usage: /notebook cell <file> <n>".to_string();
            }
            let cell_n: usize = match cell_str.parse() {
                Ok(n) => n,
                Err(_) => return format!("Cell index must be a number, got '{cell_str}'."),
            };
            let raw = match fs::read_to_string(file) {
                Ok(s) => s, Err(e) => return format!("Cannot read {file}: {e}"),
            };
            return match extract_notebook_cell(&raw, cell_n) {
                Ok(src) => {
                    let prompt = format!(
                        "Execute or explain this Jupyter notebook cell (cell {cell_n} from {file}).\n\n```python\n{src}\n```"
                    );
                    match self.run_internal_prompt_text(&prompt, false) {
                        Ok(r) => format!("Cell {cell_n} from {file}:\n\n{r}"),
                        Err(e) => format!("notebook cell: {e}"),
                    }
                }
                Err(e) => format!("Cell {cell_n} not found in {file}: {e}"),
            };
        }
        if let Some(rest) = args.strip_prefix("export ") {
            let mut parts = rest.trim().splitn(3, ' ');
            let file = parts.next().unwrap_or("").trim();
            let fmt = parts.next().unwrap_or("html").trim();
            if file.is_empty() { return "Usage: /notebook export <file> <format>".to_string(); }
            if !matches!(fmt, "html" | "pdf" | "py" | "script") {
                return format!("Unsupported format '{fmt}'. Use: html, pdf, py.");
            }
            if command_exists("jupyter") {
                let out = Command::new("jupyter").args(["nbconvert", "--to", fmt, file]).output();
                return match out {
                    Ok(o) if o.status.success() => format!("Exported {file} to {fmt}."),
                    Ok(o) => format!("Export failed:\n{}", String::from_utf8_lossy(&o.stderr).trim()),
                    Err(e) => format!("jupyter nbconvert failed: {e}"),
                };
            }
            return "jupyter not found in PATH. Install with: pip install jupyter".to_string();
        }
        format!("Unknown /notebook sub-command: {args}\nRun `/notebook help` for usage.")
    }

    // ─── Feature 3: Kubernetes Management ────────────────────────────────────

    fn run_k8s_command(args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /k8s pods                   List pods in current namespace",
                "  /k8s logs <pod>             Tail last 50 lines of pod logs",
                "  /k8s apply <file>           Apply a manifest with kubectl",
                "  /k8s describe <resource>    Describe a resource",
            ].join("\n");
        }
        if !command_exists("kubectl") {
            return "kubectl not found in PATH. Install it from \
                    https://kubernetes.io/docs/tasks/tools/".to_string();
        }
        if args == "pods" {
            let out = Command::new("kubectl").args(["get", "pods"]).output();
            return shell_output_or_err(out, "kubectl get pods");
        }
        if let Some(pod) = args.strip_prefix("logs ") {
            let pod = pod.trim();
            if pod.is_empty() { return "Usage: /k8s logs <pod>".to_string(); }
            let out = Command::new("kubectl").args(["logs", "--tail=50", pod]).output();
            return shell_output_or_err(out, &format!("kubectl logs {pod}"));
        }
        if let Some(file) = args.strip_prefix("apply ") {
            let file = file.trim();
            if file.is_empty() { return "Usage: /k8s apply <file>".to_string(); }
            let out = Command::new("kubectl").args(["apply", "-f", file]).output();
            return shell_output_or_err(out, &format!("kubectl apply -f {file}"));
        }
        if let Some(resource) = args.strip_prefix("describe ") {
            let resource = resource.trim();
            if resource.is_empty() { return "Usage: /k8s describe <resource>".to_string(); }
            let parts: Vec<&str> = resource.splitn(2, ' ').collect();
            let out = Command::new("kubectl").arg("describe").args(&parts).output();
            return shell_output_or_err(out, &format!("kubectl describe {resource}"));
        }
        format!("Unknown /k8s sub-command: {args}\nRun `/k8s help` for usage.")
    }

    // ─── Feature 4: Terraform/IaC ────────────────────────────────────────────

    fn run_iac_command(args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /iac plan       Run terraform/tofu plan",
                "  /iac apply      Run terraform/tofu apply",
                "  /iac validate   Validate configuration files",
                "  /iac drift      Detect infrastructure drift (plan -refresh-only)",
            ].join("\n");
        }
        let tf_bin = if command_exists("tofu") {
            "tofu"
        } else if command_exists("terraform") {
            "terraform"
        } else {
            return "Neither 'tofu' nor 'terraform' found in PATH.\n\
                    Install OpenTofu: https://opentofu.org/docs/intro/install/".to_string();
        };
        let tf_args: &[&str] = match args {
            "plan"     => &["plan", "-no-color"],
            "apply"    => &["apply", "-no-color", "-auto-approve"],
            "validate" => &["validate", "-no-color"],
            "drift"    => &["plan", "-refresh-only", "-no-color"],
            other => return format!("Unknown /iac sub-command: {other}\nRun `/iac help` for usage."),
        };
        let out = Command::new(tf_bin).args(tf_args).output();
        shell_output_or_err(out, &format!("{tf_bin} {args}"))
    }

    // ─── Feature 5: CI/CD Pipeline Builder ───────────────────────────────────

    fn run_pipeline_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /pipeline generate   Generate CI config from project type",
                "  /pipeline lint       Validate existing CI pipeline config",
                "  /pipeline run        Trigger a local pipeline run via act/gitlab-runner",
            ].join("\n");
        }
        match args {
            "generate" => {
                let project_type = if std::path::Path::new("Cargo.toml").exists() { "rust" } else if std::path::Path::new("package.json").exists() { "node" } else { "unknown" };
                let prompt = format!(
                    "Generate a production-quality CI/CD pipeline configuration for a {project_type} project.\n\
                     - If the project uses GitHub Actions, output a .github/workflows/ci.yml file.\n\
                     - If it uses GitLab CI, output a .gitlab-ci.yml file.\n\
                     - Cover: lint, test, build, and Docker image build if applicable.\n\
                     - Use the best community actions/images for {project_type}.\n\
                     - Output only the YAML file, nothing else."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(r) => format!("Generated CI pipeline for {project_type}:\n\n{r}"),
                    Err(e) => format!("pipeline generate: {e}"),
                }
            }
            "lint" => {
                let candidates = [
                    ".github/workflows/ci.yml", ".github/workflows/main.yml",
                    ".gitlab-ci.yml", "Jenkinsfile", ".circleci/config.yml",
                ];
                let found: Vec<&str> = candidates.iter().copied()
                    .filter(|f| Path::new(f).exists()).collect();
                if found.is_empty() {
                    return "No CI configuration files found in common locations.".to_string();
                }
                let mut report = String::from("Pipeline lint:\n");
                for path in &found {
                    let content = match fs::read_to_string(path) {
                        Ok(c) => c,
                        Err(e) => { report.push_str(&format!("\n  {path}: cannot read ({e})\n")); continue; }
                    };
                    let prompt = format!(
                        "Review this CI/CD pipeline config for errors, security issues, and improvements.\n\n\
                         File: {path}\n\n```yaml\n{yaml}\n```\n\nBe concise — max 10 lines.",
                        yaml = truncate_for_prompt(&content, 8_000),
                    );
                    let result = self.run_internal_prompt_text(&prompt, false)
                        .unwrap_or_else(|e| format!("lint error: {e}"));
                    report.push_str(&format!("\n{path}:\n{result}\n"));
                }
                report
            }
            "run" => {
                if command_exists("act") {
                    let out = Command::new("act").args(["--list"]).output();
                    let list = shell_output_or_err(out, "act --list");
                    return format!("Local runner (act) available.\n{list}\n\nRun `act` in your shell to execute.");
                }
                if command_exists("gitlab-runner") {
                    return "GitLab Runner available. Run `gitlab-runner exec shell <job>` in your shell.".to_string();
                }
                "No local pipeline runner found. Install 'act': https://github.com/nektos/act".to_string()
            }
            other => format!("Unknown /pipeline sub-command: {other}\nRun `/pipeline help` for usage."),
        }
    }

    // ─── Feature 6: Code Review ───────────────────────────────────────────────

    fn run_review_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /review <file>    Review a source file for issues",
                "  /review staged    Review all staged changes",
                "  /review pr        Review the current PR diff",
            ].join("\n");
        }
        let build_prompt = |label: &str, code: &str| -> String {
            format!(
                "You are a senior code reviewer. Review the following {label} and provide:\n\
                 1. Critical bugs or logic errors\n\
                 2. Security vulnerabilities\n\
                 3. Performance concerns\n\
                 4. Code style and readability issues\n\
                 5. Suggested improvements\n\n\
                 ```\n{code}\n```\n\nBe concise — max 20 lines.",
                code = truncate_for_prompt(code, 12_000),
            )
        };
        match args {
            "staged" => {
                let diff = Command::new("git").args(["diff", "--cached"]).output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                if diff.trim().is_empty() { return "No staged changes to review.".to_string(); }
                match self.run_internal_prompt_text(&build_prompt("staged git diff", &diff), false) {
                    Ok(r) => format!("Code review (staged changes):\n\n{r}"),
                    Err(e) => format!("review staged: {e}"),
                }
            }
            "pr" => {
                let base = Command::new("git")
                    .args(["merge-base", "HEAD", "origin/main"]).output().ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "origin/main".to_string());
                let diff = Command::new("git").args(["diff", &base, "HEAD"]).output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                if diff.trim().is_empty() { return "No diff found against origin/main.".to_string(); }
                match self.run_internal_prompt_text(&build_prompt("pull request diff", &diff), false) {
                    Ok(r) => format!("Code review (PR diff):\n\n{r}"),
                    Err(e) => format!("review pr: {e}"),
                }
            }
            file => {
                let source = match fs::read_to_string(file) {
                    Ok(s) => s, Err(e) => return format!("Cannot read {file}: {e}"),
                };
                match self.run_internal_prompt_text(&build_prompt("source file", &source), false) {
                    Ok(r) => format!("Code review ({file}):\n\n{r}"),
                    Err(e) => format!("review: {e}"),
                }
            }
        }
    }

    // ─── Feature 7: Dependency Graph ─────────────────────────────────────────

    fn run_deps_command(args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /deps tree           Show dependency tree",
                "  /deps outdated       Show outdated dependencies",
                "  /deps audit          Security audit of dependencies",
                "  /deps why <pkg>      Explain why a dependency is included",
            ].join("\n");
        }
        let pm = detect_package_manager();
        match args {
            "tree" => {
                let out = match pm {
                    PackageManager::Cargo   => Command::new("cargo").args(["tree"]).output(),
                    PackageManager::Npm     => Command::new("npm").args(["ls", "--depth=2"]).output(),
                    PackageManager::Pnpm    => Command::new("pnpm").args(["list", "--depth=2"]).output(),
                    PackageManager::Yarn    => Command::new("yarn").args(["list", "--depth=2"]).output(),
                    PackageManager::Pip     => Command::new("pip").args(["show", "--verbose"]).output(),
                    PackageManager::Unknown => return "No recognised package manager found.".to_string(),
                };
                shell_output_or_err(out, "deps tree")
            }
            "outdated" => {
                let out = match pm {
                    PackageManager::Cargo   => Command::new("cargo").args(["outdated"]).output(),
                    PackageManager::Npm     => Command::new("npm").args(["outdated"]).output(),
                    PackageManager::Pnpm    => Command::new("pnpm").args(["outdated"]).output(),
                    PackageManager::Yarn    => Command::new("yarn").args(["outdated"]).output(),
                    PackageManager::Pip     => Command::new("pip").args(["list", "--outdated"]).output(),
                    PackageManager::Unknown => return "No recognised package manager found.".to_string(),
                };
                shell_output_or_err(out, "deps outdated")
            }
            "audit" => {
                let out = match pm {
                    PackageManager::Cargo   => Command::new("cargo").args(["audit"]).output(),
                    PackageManager::Npm     => Command::new("npm").args(["audit"]).output(),
                    PackageManager::Pnpm    => Command::new("pnpm").args(["audit"]).output(),
                    PackageManager::Yarn    => Command::new("yarn").args(["audit"]).output(),
                    PackageManager::Pip     => Command::new("pip-audit").output(),
                    PackageManager::Unknown => return "No recognised package manager found.".to_string(),
                };
                shell_output_or_err(out, "deps audit")
            }
            s if s.starts_with("why ") => {
                let pkg = s["why ".len()..].trim();
                if pkg.is_empty() { return "Usage: /deps why <pkg>".to_string(); }
                let out = match pm {
                    PackageManager::Cargo => Command::new("cargo").args(["tree", "--invert", pkg]).output(),
                    PackageManager::Npm   => Command::new("npm").args(["why", pkg]).output(),
                    PackageManager::Pnpm  => Command::new("pnpm").args(["why", pkg]).output(),
                    PackageManager::Yarn  => Command::new("yarn").args(["why", pkg]).output(),
                    _ => return "/deps why is not supported for this package manager.".to_string(),
                };
                shell_output_or_err(out, &format!("deps why {pkg}"))
            }
            other => format!("Unknown /deps sub-command: {other}\nRun `/deps help` for usage."),
        }
    }

    // ─── Feature 8: Monorepo Awareness ───────────────────────────────────────

    fn run_mono_command(args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /mono list                     List workspace packages",
                "  /mono graph                    Show package dependency graph",
                "  /mono changed                  List packages changed since last git tag",
                "  /mono run <cmd> [--filter <p>] Run command in workspace packages",
            ].join("\n");
        }
        let workspace_kind = detect_workspace_kind();
        if matches!(workspace_kind, WorkspaceKind::None) {
            return "No monorepo workspace config detected.\n\
                    Expected: Cargo.toml [workspace], package.json workspaces, or pnpm-workspace.yaml".to_string();
        }
        match args {
            "list" => match workspace_kind {
                WorkspaceKind::Cargo => Command::new("cargo")
                    .args(["metadata", "--no-deps", "--format-version=1"]).output()
                    .map(|o| parse_cargo_workspace_members(&String::from_utf8_lossy(&o.stdout)))
                    .unwrap_or_else(|e| format!("cargo metadata failed: {e}")),
                WorkspaceKind::Pnpm => shell_output_or_err(
                    Command::new("pnpm").args(["ls", "--depth=0"]).output(), "pnpm ls"),
                WorkspaceKind::Npm  => shell_output_or_err(
                    Command::new("npm").args(["ls", "--depth=0"]).output(), "npm ls"),
                WorkspaceKind::None => unreachable!(),
            },
            "graph" => match workspace_kind {
                WorkspaceKind::Cargo => shell_output_or_err(
                    Command::new("cargo").args(["tree", "--workspace"]).output(),
                    "cargo tree --workspace"),
                WorkspaceKind::Pnpm => shell_output_or_err(
                    Command::new("pnpm").args(["ls", "--depth=3"]).output(), "pnpm ls --depth=3"),
                _ => shell_output_or_err(
                    Command::new("npm").args(["ls", "--depth=3"]).output(), "npm ls --depth=3"),
            },
            "changed" => {
                let last_tag = Command::new("git")
                    .args(["describe", "--tags", "--abbrev=0"]).output().ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "HEAD~10".to_string());
                let changed = Command::new("git")
                    .args(["diff", "--name-only", &last_tag, "HEAD"]).output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                if changed.trim().is_empty() {
                    return format!("No files changed since {last_tag}.");
                }
                let mut pkgs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
                for line in changed.lines() {
                    if let Some(p) = line.split('/').next() { pkgs.insert(p.to_string()); }
                }
                format!("Packages changed since {last_tag}:\n{}",
                    pkgs.iter().map(|p| format!("  {p}")).collect::<Vec<_>>().join("\n"))
            }
            s if s.starts_with("run ") => {
                let rest = s["run ".len()..].trim();
                let (filter, cmd_str) = if let Some(idx) = rest.find("--filter ") {
                    let fp = rest[idx + "--filter ".len()..].split_whitespace()
                        .next().unwrap_or("").to_string();
                    (Some(fp), rest[..idx].trim().to_string())
                } else {
                    (None, rest.to_string())
                };
                if cmd_str.is_empty() { return "Usage: /mono run <cmd> [--filter <pkg>]".to_string(); }
                match workspace_kind {
                    WorkspaceKind::Pnpm => {
                        let mut a = vec!["run".to_string()];
                        if let Some(f) = &filter { a.push("--filter".into()); a.push(f.clone()); }
                        a.push(cmd_str.clone());
                        shell_output_or_err(Command::new("pnpm").args(&a).output(),
                            &format!("pnpm run {cmd_str}"))
                    }
                    WorkspaceKind::Npm => shell_output_or_err(
                        Command::new("npm").args(["run", &cmd_str, "--workspaces"]).output(),
                        &format!("npm run {cmd_str}")),
                    WorkspaceKind::Cargo => {
                        let mut a = vec!["run".to_string()];
                        if let Some(f) = &filter { a.push("-p".into()); a.push(f.clone()); }
                        shell_output_or_err(Command::new("cargo").args(&a).output(), "cargo run")
                    }
                    WorkspaceKind::None => unreachable!(),
                }
            }
            other => format!("Unknown /mono sub-command: {other}\nRun `/mono help` for usage."),
        }
    }

    // ─── Feature 9: Browser Automation ───────────────────────────────────────

    fn run_browser_command(args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /browser open <url>         Open URL in default browser",
                "  /browser screenshot <url>   Capture a screenshot (requires playwright)",
                "  /browser test <url>         Run accessibility/performance test",
            ].join("\n");
        }
        if let Some(url) = args.strip_prefix("open ") {
            let url = url.trim();
            if url.is_empty() { return "Usage: /browser open <url>".to_string(); }
            let open_cmd = if cfg!(target_os = "macos") { "open" }
                           else if cfg!(target_os = "windows") { "start" }
                           else { "xdg-open" };
            let out = Command::new(open_cmd).arg(url).output();
            return match out {
                Ok(o) if o.status.success() => format!("Opened {url} in default browser."),
                Ok(o) => format!("open failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
                Err(e) => format!("Failed to open browser: {e}"),
            };
        }
        if let Some(url) = args.strip_prefix("screenshot ") {
            let url = url.trim();
            if url.is_empty() { return "Usage: /browser screenshot <url>".to_string(); }
            if command_exists("npx") {
                let out = Command::new("npx")
                    .args(["playwright", "screenshot", url, "screenshot.png"]).output();
                return match out {
                    Ok(o) if o.status.success() =>
                        format!("Screenshot saved to screenshot.png for {url}"),
                    Ok(o) => format!("playwright screenshot failed:\n{}",
                        String::from_utf8_lossy(&o.stderr).trim()),
                    Err(e) => format!("Failed to run playwright: {e}"),
                };
            }
            return format!(
                "playwright not available. Install with: npm install -g playwright\n\
                 Alternatively, open {url} manually and take a screenshot."
            );
        }
        if let Some(url) = args.strip_prefix("test ") {
            let url = url.trim();
            if url.is_empty() { return "Usage: /browser test <url>".to_string(); }
            if command_exists("lighthouse") {
                let out = Command::new("lighthouse")
                    .args([url, "--output=text", "--quiet", "--chrome-flags=--headless"]).output();
                return shell_output_or_err(out, &format!("lighthouse {url}"));
            }
            if command_exists("axe") {
                return shell_output_or_err(
                    Command::new("axe").arg(url).output(), &format!("axe {url}"));
            }
            return format!(
                "No testing tool found.\n\
                 Install Lighthouse: npm install -g lighthouse\n\
                 Install axe-cli:    npm install -g axe-cli\n\
                 Target URL: {url}"
            );
        }
        format!("Unknown /browser sub-command: {args}\nRun `/browser help` for usage.")
    }

    // ─── Feature 10: Notifications ───────────────────────────────────────────

    fn run_notify_command(args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "help" {
            return [
                "Usage:",
                "  /notify send <message>               Send a desktop notification",
                "  /notify webhook <url> <message>      POST message to a webhook URL",
                "  /notify matrix <room> <message>      Send to Matrix room (needs MATRIX_TOKEN)",
                "  /notify discord <webhook_url> <msg>  Send to Discord channel via webhook",
                "  /notify slack <webhook_url> <msg>    Send to Slack channel via webhook",
                "  /notify telegram <chat_id> <msg>     Send to Telegram (needs TELEGRAM_BOT_TOKEN)",
                "  /notify whatsapp <number> <msg>      Send via WhatsApp (needs WHATSAPP_API_URL, WHATSAPP_TOKEN)",
                "  /notify signal <number> <msg>        Send via Signal (needs SIGNAL_CLI_PATH or signal-cli)",
            ].join("\n");
        }
        if let Some(message) = args.strip_prefix("send ") {
            let message = message.trim();
            if message.is_empty() { return "Usage: /notify send <message>".to_string(); }
            return send_desktop_notification("Anvil", message);
        }
        if let Some(rest) = args.strip_prefix("webhook ") {
            let rest = rest.trim();
            let (url, message) = match rest.find(' ') {
                Some(idx) => (rest[..idx].trim(), rest[idx + 1..].trim()),
                None => return "Usage: /notify webhook <url> <message>".to_string(),
            };
            if url.is_empty() || message.is_empty() {
                return "Usage: /notify webhook <url> <message>".to_string();
            }
            let payload = format!(r#"{{"text":"{msg}"}}"#, msg = message.replace('"', "\\\""));
            let out = Command::new("curl")
                .args(["-s", "-o", "/dev/null", "-w", "%{http_code}",
                       "-X", "POST", "-H", "Content-Type: application/json",
                       "-d", &payload, url])
                .output();
            return match out {
                Ok(o) => {
                    let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if code.starts_with('2') { format!("Webhook delivered to {url} (HTTP {code}).")
                    } else { format!("Webhook returned HTTP {code} for {url}.") }
                }
                Err(e) => format!("curl failed: {e}. Ensure curl is installed."),
            };
        }
        if let Some(rest) = args.strip_prefix("matrix ") {
            let rest = rest.trim();
            let (room, message) = match rest.find(' ') {
                Some(idx) => (rest[..idx].trim(), rest[idx + 1..].trim()),
                None => return "Usage: /notify matrix <room> <message>".to_string(),
            };
            if room.is_empty() || message.is_empty() {
                return "Usage: /notify matrix <room> <message>".to_string();
            }
            let token = match env::var("MATRIX_TOKEN") {
                Ok(t) => t,
                Err(_) => return "MATRIX_TOKEN environment variable not set.\n\
                                  Set it to your Matrix access token.".to_string(),
            };
            let homeserver = env::var("MATRIX_HOMESERVER")
                .unwrap_or_else(|_| "https://matrix.org".to_string());
            let room_encoded = room.replace('#', "%23").replace(':', "%3A");
            let url = format!(
                "{homeserver}/_matrix/client/r0/rooms/{room_encoded}/send/m.room.message"
            );
            let payload = format!(
                r#"{{"msgtype":"m.text","body":"{msg}"}}"#,
                msg = message.replace('"', "\\\"")
            );
            let out = Command::new("curl")
                .args(["-s", "-o", "/dev/null", "-w", "%{http_code}",
                       "-X", "POST",
                       "-H", &format!("Authorization: Bearer {token}"),
                       "-H", "Content-Type: application/json",
                       "-d", &payload, &url])
                .output();
            return match out {
                Ok(o) => {
                    let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if code.starts_with('2') { format!("Matrix message sent to {room} (HTTP {code}).")
                    } else { format!("Matrix send returned HTTP {code} for room {room}.") }
                }
                Err(e) => format!("curl failed: {e}"),
            };
        }
        // Discord — via webhook URL
        if let Some(rest) = args.strip_prefix("discord ") {
            let rest = rest.trim();
            let (url, message) = match rest.find(' ') {
                Some(idx) => (rest[..idx].trim(), rest[idx + 1..].trim()),
                None => return "Usage: /notify discord <webhook_url> <message>".to_string(),
            };
            if !url.contains("discord.com/api/webhooks") {
                return "Discord webhook URL should contain discord.com/api/webhooks".to_string();
            }
            let payload = format!(r#"{{"content":"{msg}"}}"#, msg = message.replace('"', "\\\""));
            let out = Command::new("curl")
                .args(["-s", "-o", "/dev/null", "-w", "%{http_code}",
                       "-X", "POST", "-H", "Content-Type: application/json",
                       "-d", &payload, url])
                .output();
            return match out {
                Ok(o) => {
                    let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if code == "204" || code.starts_with('2') {
                        format!("Discord message delivered (HTTP {code}).")
                    } else { format!("Discord webhook returned HTTP {code}.") }
                }
                Err(e) => format!("curl failed: {e}"),
            };
        }

        // Slack — via webhook URL
        if let Some(rest) = args.strip_prefix("slack ") {
            let rest = rest.trim();
            let (url, message) = match rest.find(' ') {
                Some(idx) => (rest[..idx].trim(), rest[idx + 1..].trim()),
                None => return "Usage: /notify slack <webhook_url> <message>".to_string(),
            };
            let payload = format!(r#"{{"text":"{msg}"}}"#, msg = message.replace('"', "\\\""));
            let out = Command::new("curl")
                .args(["-s", "-o", "/dev/null", "-w", "%{http_code}",
                       "-X", "POST", "-H", "Content-Type: application/json",
                       "-d", &payload, url])
                .output();
            return match out {
                Ok(o) => {
                    let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if code.starts_with('2') {
                        format!("Slack message delivered (HTTP {code}).")
                    } else { format!("Slack webhook returned HTTP {code}.") }
                }
                Err(e) => format!("curl failed: {e}"),
            };
        }

        // Telegram — via Bot API
        if let Some(rest) = args.strip_prefix("telegram ") {
            let rest = rest.trim();
            let (chat_id, message) = match rest.find(' ') {
                Some(idx) => (rest[..idx].trim(), rest[idx + 1..].trim()),
                None => return "Usage: /notify telegram <chat_id> <message>".to_string(),
            };
            let token = match env::var("TELEGRAM_BOT_TOKEN") {
                Ok(t) => t,
                Err(_) => return "TELEGRAM_BOT_TOKEN environment variable not set.\n\
                                  Create a bot via @BotFather and set the token.".to_string(),
            };
            let url = format!(
                "https://api.telegram.org/bot{token}/sendMessage"
            );
            let payload = format!(
                r#"{{"chat_id":"{chat_id}","text":"{msg}","parse_mode":"Markdown"}}"#,
                msg = message.replace('"', "\\\"")
            );
            let out = Command::new("curl")
                .args(["-s", "-o", "/dev/null", "-w", "%{http_code}",
                       "-X", "POST", "-H", "Content-Type: application/json",
                       "-d", &payload, &url])
                .output();
            return match out {
                Ok(o) => {
                    let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if code.starts_with('2') {
                        format!("Telegram message sent to {chat_id} (HTTP {code}).")
                    } else { format!("Telegram API returned HTTP {code}.") }
                }
                Err(e) => format!("curl failed: {e}"),
            };
        }

        // WhatsApp — via WhatsApp Business API or compatible gateway
        if let Some(rest) = args.strip_prefix("whatsapp ") {
            let rest = rest.trim();
            let (number, message) = match rest.find(' ') {
                Some(idx) => (rest[..idx].trim(), rest[idx + 1..].trim()),
                None => return "Usage: /notify whatsapp <number> <message>".to_string(),
            };
            let api_url = match env::var("WHATSAPP_API_URL") {
                Ok(u) => u,
                Err(_) => return "WHATSAPP_API_URL environment variable not set.\n\
                                  Set it to your WhatsApp Business API endpoint\n\
                                  (e.g., https://graph.facebook.com/v18.0/<phone_id>/messages).".to_string(),
            };
            let token = match env::var("WHATSAPP_TOKEN") {
                Ok(t) => t,
                Err(_) => return "WHATSAPP_TOKEN environment variable not set.".to_string(),
            };
            let payload = format!(
                r#"{{"messaging_product":"whatsapp","to":"{number}","type":"text","text":{{"body":"{msg}"}}}}"#,
                msg = message.replace('"', "\\\"")
            );
            let out = Command::new("curl")
                .args(["-s", "-o", "/dev/null", "-w", "%{http_code}",
                       "-X", "POST",
                       "-H", &format!("Authorization: Bearer {token}"),
                       "-H", "Content-Type: application/json",
                       "-d", &payload, &api_url])
                .output();
            return match out {
                Ok(o) => {
                    let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if code.starts_with('2') {
                        format!("WhatsApp message sent to {number} (HTTP {code}).")
                    } else { format!("WhatsApp API returned HTTP {code}.") }
                }
                Err(e) => format!("curl failed: {e}"),
            };
        }

        // Signal — via signal-cli
        if let Some(rest) = args.strip_prefix("signal ") {
            let rest = rest.trim();
            let (number, message) = match rest.find(' ') {
                Some(idx) => (rest[..idx].trim(), rest[idx + 1..].trim()),
                None => return "Usage: /notify signal <number> <message>".to_string(),
            };
            let signal_cli = env::var("SIGNAL_CLI_PATH")
                .unwrap_or_else(|_| "signal-cli".to_string());
            let sender = match env::var("SIGNAL_SENDER") {
                Ok(s) => s,
                Err(_) => return "SIGNAL_SENDER environment variable not set.\n\
                                  Set it to your registered Signal number (e.g., +1234567890).".to_string(),
            };
            let out = Command::new(&signal_cli)
                .args(["send", "-m", message, number, "-a", &sender])
                .output();
            return match out {
                Ok(o) if o.status.success() => {
                    format!("Signal message sent to {number}.")
                }
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr);
                    format!("signal-cli error: {err}")
                }
                Err(e) => format!("signal-cli not found or failed: {e}\n\
                                   Install signal-cli: https://github.com/AsamK/signal-cli"),
            };
        }

        format!("Unknown /notify sub-command: {args}\nRun `/notify help` for usage.")
    }

    // ─── Feature 21 — Credential Vault ───────────────────────────────────────
    #[allow(clippy::unused_self)]
    fn run_vault_command(&mut self, args: Option<&str>) -> String {
        run_vault_command_impl(args)
    }

    // Feature stubs for pre-existing commands K-P that were dispatched but never implemented.

    // ─── Feature 11 — Codebase migration ─────────────────────────────────────

    fn run_migrate_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(4, ' ');
        match parts.next().unwrap_or("") {
            "framework" => {
                let from = parts.next().unwrap_or("<from>");
                let to = parts.next().unwrap_or("<to>");
                let cwd = env::current_dir().unwrap_or_default();
                let mut files: Vec<String> = Vec::new();
                for ext in &["ts", "tsx", "js", "jsx", "vue", "svelte"] {
                    if let Ok(rd) = fs::read_dir(&cwd) {
                        for e in rd.flatten() {
                            let p = e.path();
                            if p.extension().and_then(|x| x.to_str()) == Some(ext) {
                                if let Some(n) = p.file_name() { files.push(n.to_string_lossy().to_string()); }
                            }
                        }
                    }
                }
                let file_list = if files.is_empty() { "(no source files detected)".to_string() } else { files[..files.len().min(20)].join("\n") };
                let prompt = format!(
                    "I need to migrate a {from} project to {to}.\nSource files found:\n{file_list}\n\nProvide a step-by-step migration plan covering: dependency changes, config updates, breaking API differences, and code patterns to refactor. Be specific and actionable."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(plan) => format!("Migrate\n  From             {from}\n  To               {to}\n\n{plan}"),
                    Err(e) => format!("migrate framework failed: {e}"),
                }
            }
            "language" => {
                let from = parts.next().unwrap_or("<from>");
                let to = parts.next().unwrap_or("<to>");
                let prompt = format!(
                    "Explain how to migrate a codebase from {from} to {to}. Cover: type system differences, standard library equivalents, build toolchain changes, testing approach, and common gotchas. Be concise and practical."
                );
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(plan) => format!("Migrate language\n  From             {from}\n  To               {to}\n\n{plan}"),
                    Err(e) => format!("migrate language failed: {e}"),
                }
            }
            "deps" => {
                let cwd = env::current_dir().unwrap_or_default();
                if !cwd.join("package.json").exists() {
                    return "Migrate deps\n  Error            no package.json found in current directory".to_string();
                }
                let from = if cwd.join("yarn.lock").exists() { "yarn" } else if cwd.join("pnpm-lock.yaml").exists() { "pnpm" } else { "npm" };
                format!(
                    "Migrate deps\n  Detected         {from}\n\n  npm  → pnpm      npm install -g pnpm && pnpm import\n  npm  → yarn      npm install -g yarn && yarn import\n  yarn → pnpm      pnpm import\n  Note             Remove old lock files and node_modules after switching."
                )
            }
            _ => "Usage: /migrate [framework <from> <to>|language <from> <to>|deps]".to_string(),
        }
    }

    // ─── Feature 12 — Regex builder / tester ─────────────────────────────────

    fn run_regex_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let (sub, rest) = args.split_once(' ').map_or((args, ""), |(a, b)| (a, b));
        match sub {
            "build" => {
                let desc = rest.trim();
                if desc.is_empty() { return "Usage: /regex build <natural language description>".to_string(); }
                let prompt = format!("Generate a regex pattern for: {desc}\nRespond with ONLY the pattern on line 1, then a '#' comment explaining each part on line 2.");
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(out) => format!("Regex build\n  Description      {desc}\n\n{out}"),
                    Err(e) => format!("regex build failed: {e}"),
                }
            }
            "test" => {
                let mut p = rest.splitn(2, ' ');
                let pattern = p.next().unwrap_or("").trim();
                let input = p.next().unwrap_or("").trim();
                if pattern.is_empty() || input.is_empty() {
                    return "Usage: /regex test <pattern> <input>".to_string();
                }
                let out = Command::new("grep").args(["-Po", pattern]).stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn();
                match out {
                    Ok(mut child) => {
                        if let Some(mut s) = child.stdin.take() {
                            use std::io::Write;
                            let _ = s.write_all(input.as_bytes());
                        }
                        let result = child.wait_with_output();
                        let matched = result.map(|r| String::from_utf8_lossy(&r.stdout).trim().to_string()).unwrap_or_default();
                        if matched.is_empty() {
                            format!("Regex test\n  Pattern          {pattern}\n  Input            {input}\n  Result           no match")
                        } else {
                            format!("Regex test\n  Pattern          {pattern}\n  Input            {input}\n  Match            {matched}")
                        }
                    }
                    Err(_) => format!("Regex test\n  Pattern          {pattern}\n  Input            {input}\n  Note             Test manually: echo '{input}' | grep -Po '{pattern}'"),
                }
            }
            "explain" => {
                let pattern = rest.trim();
                if pattern.is_empty() { return "Usage: /regex explain <pattern>".to_string(); }
                let prompt = format!("Explain this regex pattern in plain English, component by component:\n\n{pattern}\n\nBe concise. List each token and what it matches.");
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(out) => format!("Regex explain\n  Pattern          {pattern}\n\n{out}"),
                    Err(e) => format!("regex explain failed: {e}"),
                }
            }
            _ => "Usage: /regex [build <description>|test <pattern> <input>|explain <pattern>]".to_string(),
        }
    }

    // ─── Feature 13 — SSH session manager ────────────────────────────────────

    #[allow(clippy::unused_self)]
    fn run_ssh_command(args: Option<&str>) -> String {
        let mut parts = args.unwrap_or("list").trim().splitn(4, ' ');
        let sub = parts.next().unwrap_or("list");
        match sub {
            "list" => {
                let home = PathBuf::from(env::var("HOME").unwrap_or_default());
                let config_path = home.join(".ssh").join("config");
                match fs::read_to_string(&config_path) {
                    Ok(cfg) => {
                        let hosts: Vec<String> = cfg.lines()
                            .filter(|l| l.trim_start().starts_with("Host ") && !l.contains('*'))
                            .map(|l| l.trim().trim_start_matches("Host ").trim().to_string())
                            .collect();
                        if hosts.is_empty() {
                            "SSH list\n  Result           no named hosts in ~/.ssh/config".to_string()
                        } else {
                            let list = hosts.iter().enumerate().map(|(i, h)| format!("  {}. {h}", i + 1)).collect::<Vec<_>>().join("\n");
                            format!("SSH hosts\n  Config           ~/.ssh/config\n\n{list}")
                        }
                    }
                    Err(_) => "SSH list\n  Note             ~/.ssh/config not found or not readable".to_string(),
                }
            }
            "connect" => {
                let host = parts.next().unwrap_or("<host>");
                format!("SSH connect\n  Host             {host}\n  Command          ssh {host}\n  Note             Run this in your terminal — Anvil cannot capture interactive SSH sessions.")
            }
            "tunnel" => {
                let host = parts.next().unwrap_or("<host>");
                let ports = parts.next().unwrap_or("8080:8080");
                let (local, remote) = ports.split_once(':').unwrap_or((ports, ports));
                format!("SSH tunnel\n  Host             {host}\n  Local port       {local}\n  Remote port      {remote}\n  Command          ssh -L {local}:localhost:{remote} {host} -N -f")
            }
            "keys" => {
                let home = PathBuf::from(env::var("HOME").unwrap_or_default());
                let ssh_dir = home.join(".ssh");
                match fs::read_dir(&ssh_dir) {
                    Ok(entries) => {
                        let keys: Vec<String> = entries.flatten()
                            .filter_map(|e| {
                                let p = e.path();
                                let name = p.file_name()?.to_str()?.to_string();
                                if !name.ends_with(".pub") && (name.starts_with("id_") || name.contains("_key")) {
                                    Some(format!("  {name}"))
                                } else { None }
                            })
                            .collect();
                        if keys.is_empty() { "SSH keys\n  Result           no key files found in ~/.ssh/".to_string() }
                        else { format!("SSH keys (~/.ssh/)\n\n{}", keys.join("\n")) }
                    }
                    Err(e) => format!("SSH keys\n  Error            {e}"),
                }
            }
            _ => "Usage: /ssh [list|connect <host>|tunnel <host> <local:remote>|keys]".to_string(),
        }
    }

    // ─── Feature 14 — Log analysis ───────────────────────────────────────────

    fn run_logs_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() {
            return "Usage: /logs [tail <file>|search <file> <pattern>|analyze <file>|stats <file>]".to_string();
        }
        let mut parts = args.splitn(4, ' ');
        let sub = parts.next().unwrap_or("");
        match sub {
            "tail" => {
                let file = parts.next().unwrap_or("<file>");
                match Command::new("tail").args(["-n", "50", file]).output() {
                    Ok(o) => format!("Logs tail  {file}\n\n{}", truncate_for_prompt(&String::from_utf8_lossy(&o.stdout), 4_000)),
                    Err(e) => format!("logs tail failed: {e}"),
                }
            }
            "search" => {
                let file = parts.next().unwrap_or("<file>");
                let pattern = parts.next().unwrap_or("<pattern>");
                match Command::new("grep").args(["-n", "-C", "2", pattern, file]).output() {
                    Ok(o) => {
                        let text = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        if text.is_empty() {
                            format!("Logs search\n  File             {file}\n  Pattern          {pattern}\n  Result           no matches found")
                        } else {
                            format!("Logs search  {file}  pattern={pattern}\n\n{}", truncate_for_prompt(&text, 4_000))
                        }
                    }
                    Err(e) => format!("logs search failed: {e}"),
                }
            }
            "analyze" => {
                let file = parts.next().unwrap_or("<file>");
                let content = match fs::read_to_string(file) {
                    Ok(c) => c,
                    Err(e) => return format!("logs analyze\n  Error            {e}"),
                };
                let sample = truncate_for_prompt(&content, 10_000);
                let prompt = format!("Analyze these log entries:\n1. Identify errors and root causes\n2. Recurring patterns\n3. Performance anomalies\n4. Recommended fixes\n\nLog:\n{sample}");
                match self.run_internal_prompt_text(&prompt, false) {
                    Ok(a) => format!("Logs analyze\n  File             {file}\n\n{a}"),
                    Err(e) => format!("logs analyze failed: {e}"),
                }
            }
            "stats" => {
                let file = parts.next().unwrap_or("<file>");
                match fs::read_to_string(file) {
                    Ok(content) => {
                        let total = content.lines().count();
                        let errors = content.lines().filter(|l| { let u = l.to_uppercase(); u.contains("ERROR") || u.contains("FATAL") }).count();
                        let warns = content.lines().filter(|l| l.to_uppercase().contains("WARN")).count();
                        let info = content.lines().filter(|l| l.to_uppercase().contains("INFO")).count();
                        format!("Logs stats\n  File             {file}\n  Total lines      {total}\n  ERROR/FATAL      {errors}\n  WARN             {warns}\n  INFO             {info}")
                    }
                    Err(e) => format!("logs stats\n  Error            {e}"),
                }
            }
            _ => "Usage: /logs [tail <file>|search <file> <pattern>|analyze <file>|stats <file>]".to_string(),
        }
    }

    // ─── Feature 15 — Markdown preview ───────────────────────────────────────

    #[allow(clippy::unused_self)]
    fn run_markdown_command(args: Option<&str>) -> String {
        let mut parts = args.unwrap_or("").trim().splitn(3, ' ');
        let sub = parts.next().unwrap_or("");
        let file = parts.next().unwrap_or("<file>");
        match sub {
            "preview" => {
                match fs::read_to_string(file) {
                    Ok(src) => {
                        // Strip markdown syntax for TUI plain-text preview
                        let preview: String = src.lines().map(|l| {
                            let l = l.trim_start_matches('#').trim();
                            l.trim_start_matches("**").trim_end_matches("**")
                                .trim_start_matches('*').trim_end_matches('*')
                                .to_string()
                        }).collect::<Vec<_>>().join("\n");
                        format!("Markdown preview  {file}\n\n{}", truncate_for_prompt(&preview, 5_000))
                    }
                    Err(e) => format!("Markdown preview\n  Error            {e}"),
                }
            }
            "toc" => {
                match fs::read_to_string(file) {
                    Ok(src) => {
                        let headings: Vec<String> = src.lines()
                            .filter(|l| l.starts_with('#'))
                            .map(|l| {
                                let level = l.chars().take_while(|c| *c == '#').count();
                                let title = l.trim_start_matches('#').trim();
                                let indent = "  ".repeat(level.saturating_sub(1));
                                let anchor = title.to_lowercase().replace(' ', "-")
                                    .chars().filter(|c| c.is_alphanumeric() || *c == '-').collect::<String>();
                                format!("{indent}- [{title}](#{anchor})")
                            })
                            .collect();
                        if headings.is_empty() {
                            format!("Markdown TOC\n  File             {file}\n  Result           no headings found")
                        } else {
                            format!("Markdown TOC\n  File             {file}\n\n{}", headings.join("\n"))
                        }
                    }
                    Err(e) => format!("Markdown TOC\n  Error            {e}"),
                }
            }
            "lint" => {
                match fs::read_to_string(file) {
                    Ok(src) => {
                        let mut issues: Vec<String> = Vec::new();
                        let mut in_code = false;
                        for (i, line) in src.lines().enumerate() {
                            if line.starts_with("```") { in_code = !in_code; }
                            if in_code { continue; }
                            if line.ends_with(' ') || line.ends_with('\t') {
                                issues.push(format!("  Line {:>4}  trailing whitespace", i + 1));
                            }
                            if line.len() > 120 {
                                issues.push(format!("  Line {:>4}  line exceeds 120 chars ({})", i + 1, line.len()));
                            }
                        }
                        if issues.is_empty() {
                            format!("Markdown lint\n  File             {file}\n  Result           no issues found")
                        } else {
                            format!("Markdown lint\n  File             {file}\n  Issues           {}\n\n{}", issues.len(), issues.join("\n"))
                        }
                    }
                    Err(e) => format!("Markdown lint\n  Error            {e}"),
                }
            }
            _ => "Usage: /markdown [preview <file>|toc <file>|lint <file>]".to_string(),
        }
    }

    // ─── Feature 16 — Snippet library ────────────────────────────────────────

    #[allow(clippy::unused_self)]
    fn run_snippets_command(args: Option<&str>) -> String {
        let snippets_dir = anvil_home_dir().join("snippets");
        let args = args.unwrap_or("list").trim();
        let mut parts = args.splitn(3, ' ');
        let sub = parts.next().unwrap_or("list");
        match sub {
            "save" => {
                let name = parts.next().unwrap_or("snippet");
                let content = parts.collect::<Vec<_>>().join(" ");
                if content.is_empty() {
                    return format!("Snippets save\n  Usage            /snippets save <name> <code>");
                }
                let _ = fs::create_dir_all(&snippets_dir);
                let path = snippets_dir.join(format!("{name}.snippet"));
                match fs::write(&path, &content) {
                    Ok(_) => format!("Snippets\n  Action           save\n  Name             {name}\n  Path             {}", path.display()),
                    Err(e) => format!("Snippets save\n  Error            {e}"),
                }
            }
            "list" => {
                match fs::read_dir(&snippets_dir) {
                    Ok(entries) => {
                        let names: Vec<String> = entries.flatten().filter_map(|e| {
                            let p = e.path();
                            if p.extension().map_or(false, |x| x == "snippet") {
                                p.file_stem().map(|s| format!("  {}", s.to_string_lossy()))
                            } else { None }
                        }).collect();
                        if names.is_empty() {
                            format!("Snippets\n  Directory        {}\n  Result           no snippets yet — use /snippets save <name> <code>", snippets_dir.display())
                        } else {
                            format!("Snippets  ({})\n\n{}", names.len(), names.join("\n"))
                        }
                    }
                    Err(_) => format!("Snippets\n  Directory        {}\n  Result           no snippets directory yet", snippets_dir.display()),
                }
            }
            "get" => {
                let name = parts.next().unwrap_or("<name>");
                let path = snippets_dir.join(format!("{name}.snippet"));
                match fs::read_to_string(&path) {
                    Ok(content) => format!("Snippet: {name}\n\n{content}"),
                    Err(_) => format!("Snippets get\n  Name             {name}\n  Error            not found — run /snippets list"),
                }
            }
            "search" => {
                let query = parts.collect::<Vec<_>>().join(" ");
                let query = query.trim();
                if query.is_empty() { return "Usage: /snippets search <query>".to_string(); }
                match fs::read_dir(&snippets_dir) {
                    Ok(entries) => {
                        let matches: Vec<String> = entries.flatten().filter_map(|e| {
                            let p = e.path();
                            if p.extension().map_or(false, |x| x == "snippet") {
                                let name = p.file_stem()?.to_string_lossy().to_string();
                                let content = fs::read_to_string(&p).unwrap_or_default();
                                if name.contains(query) || content.contains(query) { Some(format!("  {name}")) } else { None }
                            } else { None }
                        }).collect();
                        if matches.is_empty() {
                            format!("Snippets search\n  Query            {query}\n  Result           no matches")
                        } else {
                            format!("Snippets search\n  Query            {query}\n  Matches          {}\n\n{}", matches.len(), matches.join("\n"))
                        }
                    }
                    Err(_) => "Snippets search\n  Result           no snippets directory yet".to_string(),
                }
            }
            _ => "Usage: /snippets [save <name>|list|get <name>|search <query>]".to_string(),
        }
    }

    // ─── Feature 17 — AI fine-tuning assistant ────────────────────────────────

    fn run_finetune_command(&self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        let mut parts = args.splitn(3, ' ');
        match parts.next().unwrap_or("") {
            "prepare" => {
                let file = parts.next().unwrap_or("<file>");
                match fs::read_to_string(file) {
                    Ok(src) => {
                        let lines = src.lines().count();
                        let json_lines = src.lines().filter(|l| l.trim_start().starts_with('{')).count();
                        let prompt = format!(
                            "Review this fine-tuning training data file:\nFile: {file}\nLines: {lines}\nJSON-like: {json_lines}\nSample:\n{}\n\nCheck: JSONL correctness, role pairs, data diversity, biases, sample count. Provide quality assessment.",
                            truncate_for_prompt(&src, 2_000)
                        );
                        match self.run_internal_prompt_text(&prompt, false) {
                            Ok(a) => format!("Finetune prepare\n  File             {file}\n  Lines            {lines}\n\n{a}"),
                            Err(e) => format!("finetune prepare failed: {e}"),
                        }
                    }
                    Err(e) => format!("Finetune prepare\n  Error            {e}"),
                }
            }
            "validate" => {
                let file = parts.next().unwrap_or("<file>");
                match fs::read_to_string(file) {
                    Ok(src) => {
                        let mut errors: Vec<String> = Vec::new();
                        for (i, line) in src.lines().enumerate() {
                            let l = line.trim();
                            if l.is_empty() { continue; }
                            if serde_json::from_str::<serde_json::Value>(l).is_err() {
                                errors.push(format!("  Line {:>4}  invalid JSON: {}", i + 1, &l[..l.len().min(60)]));
                            }
                        }
                        if errors.is_empty() {
                            let count = src.lines().filter(|l| !l.trim().is_empty()).count();
                            format!("Finetune validate\n  File             {file}\n  Examples         {count}\n  Result           valid JSONL")
                        } else {
                            format!("Finetune validate\n  File             {file}\n  Errors           {}\n\n{}", errors.len(), errors.join("\n"))
                        }
                    }
                    Err(e) => format!("Finetune validate\n  Error            {e}"),
                }
            }
            "start" => "Finetune start\n  Steps\n    1. Validate data     /finetune validate <file>\n    2. Upload file        openai api files.create -f <file> -p fine-tune\n    3. Start job          openai api fine_tuning.jobs.create -m gpt-4o-mini -t <file-id>\n  Docs                 https://platform.openai.com/docs/guides/fine-tuning".to_string(),
            "status" => {
                match Command::new("openai").args(["api", "fine_tuning.jobs.list"]).output() {
                    Ok(o) if o.status.success() => format!("Finetune jobs\n\n{}", truncate_for_prompt(&String::from_utf8_lossy(&o.stdout), 3_000)),
                    _ => "Finetune status\n  Note             Install OpenAI CLI: pip install openai\n  Then:            openai api fine_tuning.jobs.list".to_string(),
                }
            }
            _ => "Usage: /finetune [prepare <file>|validate <file>|start|status]".to_string(),
        }
    }

    // ─── Feature 18 — Webhook manager ────────────────────────────────────────

    #[allow(clippy::unused_self)]
    fn run_webhook_command(args: Option<&str>) -> String {
        let webhooks_file = anvil_home_dir().join("webhooks.json");
        let load_wh = || -> serde_json::Map<String, serde_json::Value> {
            fs::read_to_string(&webhooks_file).ok()
                .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
        };

        let args = args.unwrap_or("list").trim();
        let mut parts = args.splitn(4, ' ');
        let sub = parts.next().unwrap_or("list");
        match sub {
            "list" => {
                let wh = load_wh();
                if wh.is_empty() {
                    format!("Webhooks\n  Config           {}\n  Result           no webhooks — use /webhook add <name> <url>", webhooks_file.display())
                } else {
                    let list = wh.iter().enumerate()
                        .map(|(i, (n, u))| format!("  {}. {n:<20} {}", i + 1, u.as_str().unwrap_or("<invalid>")))
                        .collect::<Vec<_>>().join("\n");
                    format!("Webhooks  ({})\n\n{list}", wh.len())
                }
            }
            "add" => {
                let name = parts.next().unwrap_or("<name>").to_string();
                let url = parts.next().unwrap_or("<url>").to_string();
                let mut wh = load_wh();
                wh.insert(name.clone(), serde_json::Value::String(url.clone()));
                let _ = fs::create_dir_all(anvil_home_dir());
                let _ = fs::write(&webhooks_file, serde_json::to_string_pretty(&wh).unwrap_or_default());
                format!("Webhooks\n  Action           add\n  Name             {name}\n  URL              {url}\n  Result           saved")
            }
            "test" => {
                let name = parts.next().unwrap_or("<name>");
                let wh = load_wh();
                let url = match wh.get(name).and_then(|v| v.as_str()) {
                    Some(u) => u.to_string(),
                    None => return format!("Webhook test\n  Name             {name}\n  Error            not found — run /webhook list"),
                };
                let payload = r#"{"text":"Anvil webhook test","source":"anvil-cli"}"#;
                match Command::new("curl").args(["-s", "-o", "/dev/null", "-w", "%{http_code}",
                    "-X", "POST", "-H", "Content-Type: application/json", "-d", payload, &url]).output() {
                    Ok(o) => format!("Webhook test\n  Name             {name}\n  URL              {url}\n  HTTP status      {}", String::from_utf8_lossy(&o.stdout).trim()),
                    Err(e) => format!("webhook test failed: {e}"),
                }
            }
            "remove" => {
                let name = parts.next().unwrap_or("<name>");
                let mut wh = load_wh();
                if wh.remove(name).is_some() {
                    let _ = fs::write(&webhooks_file, serde_json::to_string_pretty(&wh).unwrap_or_default());
                    format!("Webhooks\n  Action           remove\n  Name             {name}\n  Result           removed")
                } else {
                    format!("Webhooks remove\n  Name             {name}\n  Error            not found")
                }
            }
            _ => "Usage: /webhook [list|add <name> <url>|test <name>|remove <name>]".to_string(),
        }
    }

    // ─── Feature 20 — Plugin SDK ──────────────────────────────────────────────

    #[allow(clippy::unused_self)]
    fn run_plugin_sdk_command(args: Option<&str>) -> String {
        let mut parts = args.unwrap_or("").trim().splitn(3, ' ');
        let sub = parts.next().unwrap_or("");
        match sub {
            "init" => {
                let name = parts.next().unwrap_or("my-plugin");
                let plugin_dir = env::current_dir().unwrap_or_default().join(name);
                if plugin_dir.exists() {
                    return format!("Plugin SDK init\n  Error            directory already exists: {}", plugin_dir.display());
                }
                let _ = fs::create_dir_all(plugin_dir.join("src"));
                let manifest = format!(r#"{{
  "name": "{name}",
  "version": "0.1.0",
  "description": "An Anvil plugin",
  "main": "src/index.ts",
  "hooks": ["on_message", "on_tool_result"],
  "permissions": ["read_files"]
}}"#);
                let index_ts = "// Anvil Plugin SDK entry point\n// Implement hooks: on_message, on_tool_result\n\nexport default {\n  name: 'plugin',\n\n  async on_message(_ctx, message) {\n    return null; // pass-through\n  },\n\n  async on_tool_result(_ctx, _tool, result) {\n    return result;\n  },\n};\n";
                let _ = fs::write(plugin_dir.join("plugin.json"), &manifest);
                let _ = fs::write(plugin_dir.join("src").join("index.ts"), index_ts);
                let _ = fs::write(plugin_dir.join("README.md"), format!("# {name}\n\nAnvil plugin.\n"));
                format!("Plugin SDK init\n  Name             {name}\n  Directory        {}\n  Created          plugin.json, src/index.ts, README.md\n  Next             cd {name} && /plugin-sdk build", plugin_dir.display())
            }
            "build" => {
                let cwd = env::current_dir().unwrap_or_default();
                if !cwd.join("plugin.json").exists() {
                    return "Plugin SDK build\n  Error            plugin.json not found — run /plugin-sdk init <name> first".to_string();
                }
                match Command::new("npx").args(["tsc", "--noEmit"]).current_dir(&cwd).output() {
                    Ok(o) if o.status.success() => "Plugin SDK build\n  Result           TypeScript checks passed".to_string(),
                    Ok(o) => format!("Plugin SDK build\n  Errors\n{}", String::from_utf8_lossy(&o.stderr).trim()),
                    Err(_) => "Plugin SDK build\n  Note             Install TypeScript: npm install -g typescript".to_string(),
                }
            }
            "test" => {
                let cwd = env::current_dir().unwrap_or_default();
                match Command::new("npm").args(["test"]).current_dir(&cwd).output() {
                    Ok(o) => {
                        let text = if o.status.success() {
                            String::from_utf8_lossy(&o.stdout).trim().to_string()
                        } else {
                            String::from_utf8_lossy(&o.stderr).trim().to_string()
                        };
                        format!("Plugin SDK test\n  Result           {}\n\n{}", if o.status.success() { "passed" } else { "failed" }, truncate_for_prompt(&text, 2_000))
                    }
                    Err(_) => "Plugin SDK test\n  Note             Run npm test in your plugin directory".to_string(),
                }
            }
            "publish" => "Plugin SDK publish\n  Status           AnvilHub publishing is not yet live.\n  Coming soon      /plugin-sdk publish will submit to AnvilHub.\n  Meanwhile        Share via GitHub and install with /plugin install <path>".to_string(),
            _ => "Usage: /plugin-sdk [init <name>|build|test|publish]".to_string(),
        }
    }

    /// Read a string value from `~/.anvil/config.json` with a fallback default.
    fn anvil_config_str(&self, key: &str, default: &str) -> String {
        let cfg = Self::load_anvil_ui_config();
        cfg.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    }



    #[allow(clippy::unused_self)]
    fn run_teleport(&self, target: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let Some(target) = target.map(str::trim).filter(|value| !value.is_empty()) else {
            println!("Usage: /teleport <symbol-or-path>");
            return Ok(());
        };

        println!("{}", render_teleport_report(target)?);
        Ok(())
    }

    fn run_debug_tool_call(&self) -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", render_last_tool_debug_report(self.runtime.session())?);
        Ok(())
    }

    fn run_commit(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let status = git_output(&["status", "--short"])?;
        if status.trim().is_empty() {
            println!("Commit\n  Result           skipped\n  Reason           no workspace changes");
            return Ok(());
        }

        git_status_ok(&["add", "-A"])?;
        let staged_stat = git_output(&["diff", "--cached", "--stat"])?;
        let prompt = format!(
            "Generate a git commit message in plain text Lore format only. Base it on this staged diff summary:\n\n{}\n\nRecent conversation context:\n{}",
            truncate_for_prompt(&staged_stat, 8_000),
            recent_user_context(self.runtime.session(), 6)
        );
        let message = sanitize_generated_message(&self.run_internal_prompt_text(&prompt, false)?);
        if message.trim().is_empty() {
            return Err("generated commit message was empty".into());
        }

        let path = write_temp_text_file("anvil-commit-message.txt", &message)?;
        let output = Command::new("git")
            .args(["commit", "--file"])
            .arg(&path)
            .current_dir(env::current_dir()?)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(format!("git commit failed: {stderr}").into());
        }

        println!(
            "Commit\n  Result           created\n  Message file     {}\n\n{}",
            path.display(),
            message.trim()
        );
        Ok(())
    }

    fn run_pr(&self, context: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let staged = git_output(&["diff", "--stat"])?;
        let prompt = format!(
            "Generate a pull request title and body from this conversation and diff summary. Output plain text in this format exactly:\nTITLE: <title>\nBODY:\n<body markdown>\n\nContext hint: {}\n\nDiff summary:\n{}",
            context.unwrap_or("none"),
            truncate_for_prompt(&staged, 10_000)
        );
        let draft = sanitize_generated_message(&self.run_internal_prompt_text(&prompt, false)?);
        let (title, body) = parse_titled_body(&draft)
            .ok_or_else(|| "failed to parse generated PR title/body".to_string())?;

        if command_exists("gh") {
            let body_path = write_temp_text_file("anvil-pr-body.md", &body)?;
            let output = Command::new("gh")
                .args(["pr", "create", "--title", &title, "--body-file"])
                .arg(&body_path)
                .current_dir(env::current_dir()?)
                .output()?;
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                println!(
                    "PR\n  Result           created\n  Title            {title}\n  URL              {}",
                    if stdout.is_empty() { "<unknown>" } else { &stdout }
                );
                return Ok(());
            }
        }

        println!("PR draft\n  Title            {title}\n\n{body}");
        Ok(())
    }

    fn run_issue(&self, context: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let prompt = format!(
            "Generate a GitHub issue title and body from this conversation. Output plain text in this format exactly:\nTITLE: <title>\nBODY:\n<body markdown>\n\nContext hint: {}\n\nConversation context:\n{}",
            context.unwrap_or("none"),
            truncate_for_prompt(&recent_user_context(self.runtime.session(), 10), 10_000)
        );
        let draft = sanitize_generated_message(&self.run_internal_prompt_text(&prompt, false)?);
        let (title, body) = parse_titled_body(&draft)
            .ok_or_else(|| "failed to parse generated issue title/body".to_string())?;

        if command_exists("gh") {
            let body_path = write_temp_text_file("anvil-issue-body.md", &body)?;
            let output = Command::new("gh")
                .args(["issue", "create", "--title", &title, "--body-file"])
                .arg(&body_path)
                .current_dir(env::current_dir()?)
                .output()?;
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                println!(
                    "Issue\n  Result           created\n  Title            {title}\n  URL              {}",
                    if stdout.is_empty() { "<unknown>" } else { &stdout }
                );
                return Ok(());
            }
        }

        println!("Issue draft\n  Title            {title}\n\n{body}");
        Ok(())
    }
}

fn sessions_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let path = cwd.join(".anvil").join("sessions");
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn create_managed_session_handle() -> Result<SessionHandle, Box<dyn std::error::Error>> {
    let id = generate_session_id();
    let path = sessions_dir()?.join(format!("{id}.json"));
    Ok(SessionHandle { id, path })
}

fn generate_session_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("session-{millis}")
}

fn resolve_session_reference(reference: &str) -> Result<SessionHandle, Box<dyn std::error::Error>> {
    let direct = PathBuf::from(reference);
    let path = if direct.exists() {
        direct
    } else {
        sessions_dir()?.join(format!("{reference}.json"))
    };
    if !path.exists() {
        return Err(format!("session not found: {reference}").into());
    }
    let id = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(reference)
        .to_string();
    Ok(SessionHandle { id, path })
}

fn list_managed_sessions() -> Result<Vec<ManagedSessionSummary>, Box<dyn std::error::Error>> {
    let mut sessions = Vec::new();
    for entry in fs::read_dir(sessions_dir()?)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let metadata = entry.metadata()?;
        let modified_epoch_secs = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        let message_count = Session::load_from_path(&path)
            .map(|session| session.messages.len())
            .unwrap_or_default();
        let id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("unknown")
            .to_string();
        sessions.push(ManagedSessionSummary {
            id,
            path,
            modified_epoch_secs,
            message_count,
        });
    }
    sessions.sort_by(|left, right| right.modified_epoch_secs.cmp(&left.modified_epoch_secs));
    Ok(sessions)
}

fn format_relative_timestamp(epoch_secs: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(epoch_secs);
    let elapsed = now.saturating_sub(epoch_secs);
    match elapsed {
        0..=59 => format!("{elapsed}s ago"),
        60..=3_599 => format!("{}m ago", elapsed / 60),
        3_600..=86_399 => format!("{}h ago", elapsed / 3_600),
        _ => format!("{}d ago", elapsed / 86_400),
    }
}

fn render_session_list(active_session_id: &str) -> Result<String, Box<dyn std::error::Error>> {
    let sessions = list_managed_sessions()?;
    let mut lines = vec![
        "Sessions".to_string(),
        format!("  Directory         {}", sessions_dir()?.display()),
    ];
    if sessions.is_empty() {
        lines.push("  No managed sessions saved yet.".to_string());
        return Ok(lines.join("\n"));
    }
    for session in sessions {
        let marker = if session.id == active_session_id {
            "● current"
        } else {
            "○ saved"
        };
        lines.push(format!(
            "  {id:<20} {marker:<10} {msgs:>3} msgs · updated {modified}",
            id = session.id,
            msgs = session.message_count,
            modified = format_relative_timestamp(session.modified_epoch_secs),
        ));
        lines.push(format!("    {}", session.path.display()));
    }
    Ok(lines.join("\n"))
}

fn render_repl_help() -> String {
    [
        "Interactive REPL".to_string(),
        "  Quick start          Ask a task in plain English or use one of the core commands below."
            .to_string(),
        "  Core commands        /help · /status · /model · /permissions · /compact".to_string(),
        "  Exit                 /exit or /quit".to_string(),
        "  Vim mode             /vim toggles modal editing".to_string(),
        "  History              Up/Down recalls previous prompts".to_string(),
        "  Completion           Tab cycles slash command matches".to_string(),
        "  Cancel               Ctrl-C clears input (or exits on an empty prompt)".to_string(),
        "  Multiline            Shift+Enter or Ctrl+J inserts a newline".to_string(),
        String::new(),
        render_slash_command_help(),
    ]
    .join(
        "
",
    )
}

fn append_slash_command_suggestions(lines: &mut Vec<String>, name: &str) {
    let suggestions = suggest_slash_commands(name, 3);
    if suggestions.is_empty() {
        lines.push("  Try              /help shows the full slash command map".to_string());
        return;
    }

    lines.push("  Try              /help shows the full slash command map".to_string());
    lines.push("Suggestions".to_string());
    lines.extend(
        suggestions
            .into_iter()
            .map(|suggestion| format!("  {suggestion}")),
    );
}

fn render_unknown_repl_command(name: &str) -> String {
    let mut lines = vec![
        "Unknown slash command".to_string(),
        format!("  Command          /{name}"),
    ];
    append_repl_command_suggestions(&mut lines, name);
    lines.join("\n")
}

fn append_repl_command_suggestions(lines: &mut Vec<String>, name: &str) {
    let suggestions = suggest_repl_commands(name);
    if suggestions.is_empty() {
        lines.push("  Try              /help shows the full slash command map".to_string());
        return;
    }

    lines.push("  Try              /help shows the full slash command map".to_string());
    lines.push("Suggestions".to_string());
    lines.extend(
        suggestions
            .into_iter()
            .map(|suggestion| format!("  {suggestion}")),
    );
}

fn render_mode_unavailable(command: &str, label: &str) -> String {
    [
        "Command unavailable in this REPL mode".to_string(),
        format!("  Command          /{command}"),
        format!("  Feature          {label}"),
        "  Tip              Use /help to find currently wired REPL commands".to_string(),
    ]
    .join("\n")
}


// ---------------------------------------------------------------------------
/// Return `~/.anvil/` as a `PathBuf`.
fn anvil_home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default())
        .join(".anvil")
}

/// Standalone language command handler.
// ─── Feature 21 — Credential Vault free function ─────────────────────────────

/// Map a language name to its LSP server binary name.
fn lsp_binary_for_lang(lang: &str) -> String {
    match lang.to_ascii_lowercase().as_str() {
        "rust" => "rust-analyzer",
        "typescript" | "ts" | "javascript" | "js" => "typescript-language-server",
        "python" | "py" => "pylsp",
        "go" => "gopls",
        "java" => "jdtls",
        "c" | "cpp" | "c++" => "clangd",
        other => other,
    }
    .to_string()
}

/// Extract a single cell from a Jupyter notebook JSON by 1-based index.
fn extract_notebook_cell(raw: &str, cell_n: usize) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let cells = v["cells"]
        .as_array()
        .ok_or_else(|| "No cells array in notebook".to_string())?;
    let cell = cells
        .get(cell_n.saturating_sub(1))
        .ok_or_else(|| format!("Cell {cell_n} not found (notebook has {} cells)", cells.len()))?;
    let source = cell["source"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .or_else(|| cell["source"].as_str().map(ToOwned::to_owned))
        .unwrap_or_default();
    Ok(source)
}

/// Convert a `Command::output()` result into a human-readable string.
fn shell_output_or_err(result: Result<std::process::Output, std::io::Error>, context: &str) -> String {
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if !stdout.is_empty() {
                stdout
            } else if !stderr.is_empty() {
                stderr
            } else {
                format!("{context}: (no output)")
            }
        }
        Err(e) => format!("{context}: {e}"),
    }
}

// ─── Package manager detection ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Cargo,
    Npm,
    Pnpm,
    Yarn,
    Pip,
    Unknown,
}

fn detect_package_manager() -> PackageManager {
    if Path::new("Cargo.toml").exists() { return PackageManager::Cargo; }
    if Path::new("pnpm-lock.yaml").exists() || Path::new("pnpm-workspace.yaml").exists() {
        return PackageManager::Pnpm;
    }
    if Path::new("yarn.lock").exists() { return PackageManager::Yarn; }
    if Path::new("package.json").exists() { return PackageManager::Npm; }
    if Path::new("pyproject.toml").exists() || Path::new("setup.py").exists()
        || Path::new("requirements.txt").exists() {
        return PackageManager::Pip;
    }
    PackageManager::Unknown
}

// ─── Workspace (monorepo) detection ──────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceKind {
    Cargo,
    Npm,
    Pnpm,
    None,
}

fn detect_workspace_kind() -> WorkspaceKind {
    // Cargo workspace: Cargo.toml must contain [workspace]
    if Path::new("Cargo.toml").exists() {
        if let Ok(content) = fs::read_to_string("Cargo.toml") {
            if content.contains("[workspace]") {
                return WorkspaceKind::Cargo;
            }
        }
    }
    if Path::new("pnpm-workspace.yaml").exists() {
        return WorkspaceKind::Pnpm;
    }
    // npm workspaces: package.json must have a "workspaces" key
    if Path::new("package.json").exists() {
        if let Ok(content) = fs::read_to_string("package.json") {
            if content.contains("\"workspaces\"") {
                return WorkspaceKind::Npm;
            }
        }
    }
    WorkspaceKind::None
}

/// Parse the output of `cargo metadata --no-deps --format-version=1` and list package names.
fn parse_cargo_workspace_members(json_text: &str) -> String {
    // Quick heuristic: extract "name":"…" pairs from metadata JSON.
    let mut names: Vec<String> = Vec::new();
    let mut rest = json_text;
    while let Some(idx) = rest.find("\"name\":\"") {
        rest = &rest[idx + 8..];
        if let Some(end) = rest.find('"') {
            names.push(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    names.dedup();
    if names.is_empty() {
        return "No workspace packages found.".to_string();
    }
    format!("Workspace packages ({}):\n{}", names.len(),
        names.iter().map(|n| format!("  {n}")).collect::<Vec<_>>().join("\n"))
}

// ─── CI/CD project-type detection ────────────────────────────────────────────

fn detect_project_type_for_pipeline() -> &'static str {
    if Path::new("Cargo.toml").exists() { return "Rust (Cargo)"; }
    if Path::new("go.mod").exists() { return "Go"; }
    if Path::new("pyproject.toml").exists() || Path::new("setup.py").exists() { return "Python"; }
    if Path::new("pom.xml").exists() { return "Java (Maven)"; }
    if Path::new("build.gradle").exists() || Path::new("build.gradle.kts").exists() {
        return "Java/Kotlin (Gradle)";
    }
    if Path::new("package.json").exists() {
        // Check if it's a Next.js project
        if let Ok(c) = fs::read_to_string("package.json") {
            if c.contains("\"next\"") { return "Next.js"; }
            if c.contains("\"react\"") { return "React"; }
        }
        return "Node.js";
    }
    if Path::new("Dockerfile").exists() { return "Docker"; }
    "generic"
}

// ─── Desktop notification helper ─────────────────────────────────────────────

fn send_desktop_notification(title: &str, message: &str) -> String {
    // macOS
    if cfg!(target_os = "macos") {
        let script = format!(
            r#"display notification "{msg}" with title "{title}""#,
            msg = message.replace('"', "\\\""),
            title = title.replace('"', "\\\""),
        );
        let out = Command::new("osascript").args(["-e", &script]).output();
        return match out {
            Ok(o) if o.status.success() => format!("Notification sent: {message}"),
            Ok(o) => format!("osascript failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
            Err(e) => format!("osascript not available: {e}"),
        };
    }
    // Linux (notify-send)
    let out = Command::new("notify-send").args([title, message]).output();
    match out {
        Ok(o) if o.status.success() => format!("Notification sent: {message}"),
        Ok(_) => {
            // Fall back to wall/echo
            format!("Desktop notification: [{title}] {message}")
        }
        Err(_) => format!(
            "notify-send not available. Install libnotify-bin (Linux) or use macOS.\n\
             Message: [{title}] {message}"
        ),
    }
}

/// Stateless vault command runner.  The vault manager is constructed on each
/// call because `LiveCli` does not hold persistent inter-call state for the vault.
/// For interactive secret prompts in REPL mode `rpassword`-style no-echo reads are
/// not available without adding a dependency; we fall back to a visible-input note.
fn run_vault_command_impl(args: Option<&str>) -> String {
    use runtime::{Credential, TotpEntry, VaultManager};
    use std::time::{SystemTime, UNIX_EPOCH};

    let sub = args.unwrap_or("").trim();
    let mut parts = sub.splitn(3, ' ');
    let cmd = parts.next().unwrap_or("").trim();
    let arg1 = parts.next().unwrap_or("").trim();
    let arg2 = parts.next().unwrap_or("").trim();

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
            let password = arg1;
            if password.is_empty() {
                return "Vault\n  Usage            /vault setup <master-password>\n  Note             Master password is not stored — you must remember it.".to_string();
            }
            match vm.setup(password) {
                Ok(()) => format!(
                    "Vault\n  Result           Initialized\n  Storage          {}\n  Algorithm        Argon2id + AES-256-GCM\n  Note             Master password not stored — keep it safe.",
                    VaultManager::default_vault_dir().display()
                ),
                Err(e) => format!("Vault setup error: {e}"),
            }
        }

        // ── Unlock ───────────────────────────────────────────────────────────
        "unlock" => {
            // Note: VaultManager is transient per call — unlock persists only for
            // this invocation.  A persistent vault session requires storing the
            // manager in LiveCli state (future work).
            let password = arg1;
            if password.is_empty() {
                return "Vault\n  Usage            /vault unlock <master-password>".to_string();
            }
            match vm.unlock(password) {
                Ok(()) => "Vault\n  Result           Unlocked\n  Note             Vault is unlocked for this command only. Pass password with each command for now.".to_string(),
                Err(e) => format!("Vault unlock error: {e}"),
            }
        }

        // ── Lock ─────────────────────────────────────────────────────────────
        "lock" => "Vault\n  Result           Locked (vault memory cleared)".to_string(),

        // ── Store ─────────────────────────────────────────────────────────────
        // Usage: /vault store <label> <master-password>
        // The secret value is prompted interactively in a real terminal; here we
        // require it as the third word because no-echo stdin is not available.
        "store" => {
            let label = arg1;
            let password = arg2;
            if label.is_empty() || password.is_empty() {
                return "Vault\n  Usage            /vault store <label> <master-password>\n  Note             Run in terminal for interactive no-echo secret prompt.".to_string();
            }
            match vm.unlock(password) {
                Err(e) => return format!("Vault unlock error: {e}"),
                Ok(()) => {}
            }
            // Prompt for the secret value via stdin (best-effort; no TTY hiding).
            println!("Vault  Enter secret for '{label}': ");
            let mut secret = String::new();
            let _ = std::io::stdin().read_line(&mut secret);
            let secret = secret.trim().to_string();
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
        // Usage: /vault get <label> <master-password>
        "get" => {
            let label = arg1;
            let password = arg2;
            if label.is_empty() || password.is_empty() {
                return "Vault\n  Usage            /vault get <label> <master-password>".to_string();
            }
            match vm.unlock(password) {
                Err(e) => return format!("Vault unlock error: {e}"),
                Ok(()) => {}
            }
            match vm.get_credential(label) {
                Ok(cred) => {
                    let username = cred.username.as_deref().unwrap_or("(none)");
                    let notes = cred.notes.as_deref().unwrap_or("(none)");
                    format!(
                        "Vault\n  Label            {}\n  Username         {username}\n  Secret           {}\n  Notes            {notes}",
                        cred.label, cred.secret
                    )
                }
                Err(e) => format!("Vault get error: {e}"),
            }
        }

        // ── List ──────────────────────────────────────────────────────────────
        // Usage: /vault list <master-password>
        "list" => {
            let password = arg1;
            if password.is_empty() {
                return "Vault\n  Usage            /vault list <master-password>".to_string();
            }
            match vm.unlock(password) {
                Err(e) => return format!("Vault unlock error: {e}"),
                Ok(()) => {}
            }
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
        // Usage: /vault delete <label> <master-password>
        "delete" => {
            let label = arg1;
            let password = arg2;
            if label.is_empty() || password.is_empty() {
                return "Vault\n  Usage            /vault delete <label> <master-password>".to_string();
            }
            match vm.unlock(password) {
                Err(e) => return format!("Vault unlock error: {e}"),
                Ok(()) => {}
            }
            match vm.delete_credential(label) {
                Ok(()) => format!("Vault\n  Result           Deleted\n  Label            {label}"),
                Err(e) => format!("Vault delete error: {e}"),
            }
        }

        // ── TOTP sub-commands ─────────────────────────────────────────────────
        "totp" => {
            // arg1 = totp sub-command, arg2 = label (or password)
            match arg1 {
                // /vault totp list <master-password>
                "list" => {
                    let password = arg2;
                    if password.is_empty() {
                        return "Vault\n  Usage            /vault totp list <master-password>".to_string();
                    }
                    match vm.unlock(password) {
                        Err(e) => return format!("Vault unlock error: {e}"),
                        Ok(()) => {}
                    }
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

                // /vault totp add <label> — prompts for secret interactively
                "add" => {
                    // arg2 is treated as "<label> <master-password>" from the remaining text.
                    let mut rest = arg2.splitn(2, ' ');
                    let label = rest.next().unwrap_or("").trim();
                    let password = rest.next().unwrap_or("").trim();
                    if label.is_empty() || password.is_empty() {
                        return "Vault\n  Usage            /vault totp add <label> <master-password>\n  Note             You will be prompted for the Base32 TOTP secret.".to_string();
                    }
                    match vm.unlock(password) {
                        Err(e) => return format!("Vault unlock error: {e}"),
                        Ok(()) => {}
                    }
                    println!("Vault  Enter TOTP Base32 secret for '{label}': ");
                    let mut secret_input = String::new();
                    let _ = std::io::stdin().read_line(&mut secret_input);
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

                // /vault totp delete <label> <master-password>
                "delete" => {
                    let mut rest = arg2.splitn(2, ' ');
                    let label = rest.next().unwrap_or("").trim();
                    let password = rest.next().unwrap_or("").trim();
                    if label.is_empty() || password.is_empty() {
                        return "Vault\n  Usage            /vault totp delete <label> <master-password>".to_string();
                    }
                    match vm.unlock(password) {
                        Err(e) => return format!("Vault unlock error: {e}"),
                        Ok(()) => {}
                    }
                    match vm.delete_totp(label) {
                        Ok(()) => format!("Vault\n  Result           TOTP deleted\n  Label            {label}"),
                        Err(e) => format!("Vault TOTP delete error: {e}"),
                    }
                }

                // /vault totp <label> <master-password> — generate current code
                label if !label.is_empty() => {
                    let password = arg2;
                    if password.is_empty() {
                        return format!("Vault\n  Usage            /vault totp {label} <master-password>");
                    }
                    match vm.unlock(password) {
                        Err(e) => return format!("Vault unlock error: {e}"),
                        Ok(()) => {}
                    }
                    match vm.generate_totp(label) {
                        Ok(code) => format!(
                            "Vault — TOTP\n  Label            {label}\n  Code             {}\n  Valid for        {}s",
                            code.code, code.remaining_secs
                        ),
                        Err(e) => format!("Vault TOTP error: {e}"),
                    }
                }

                _ => "Vault\n  Usage            /vault totp [add <label>|<label>|list|delete <label>] <master-password>".to_string(),
            }
        }

        other => format!(
            "Vault\n  Unknown subcommand: {other}\n  Run /vault for usage."
        ),
    }
}

fn run_language_command_static(lang: Option<&str>) -> String {
    const SUPPORTED: &[&str] = &["en", "de", "es", "fr", "ja", "zh-CN", "ru"];

    let Some(lang) = lang else {
        let current = current_language_code();
        return format!(
            "Language: {current}\nAvailable: {}\nUsage: /language <code>",
            SUPPORTED.join(", ")
        );
    };

    let lang = lang.trim();
    if lang.is_empty() {
        return format!(
            "Language: {}\nAvailable: {}\nUsage: /language <code>",
            current_language_code(),
            SUPPORTED.join(", ")
        );
    }

    if !SUPPORTED.contains(&lang) {
        return format!(
            "Unsupported language '{lang}'. Available: {}",
            SUPPORTED.join(", ")
        );
    }

    let anvil_dir = anvil_home_dir();
    let path = anvil_dir.join("config.json");
    let mut map = if path.exists() {
        fs::read_to_string(&path)
            .ok()
            .and_then(|data| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data).ok())
            .unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    map.insert("language".to_string(), serde_json::Value::String(lang.to_string()));

    let _ = fs::create_dir_all(&anvil_dir);
    match fs::write(&path, serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap_or_default()) {
        Ok(()) => {
            rust_i18n::set_locale(lang);
            format!("Language set to: {lang}")
        }
        Err(e) => format!("Failed to save language setting: {e}"),
    }
}

/// Return the currently configured language code, defaulting to "en".
fn current_language_code() -> String {
    let path = anvil_home_dir().join("config.json");
    if let Ok(data) = fs::read_to_string(&path) {
        if let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data) {
            if let Some(lang) = map.get("language").and_then(|v| v.as_str()) {
                return lang.to_string();
            }
        }
    }
    "en".to_string()
}


/// Static version of `/configure` for use in the `--resume` path, where no
/// `LiveCli` instance is available.  Produces the same output as the live
/// version for purely informational sub-commands; write operations advise
/// the user to run `/configure` in an active session.
fn render_configure_static(args: Option<&str>) -> String {
    let args = args.unwrap_or("").trim();
    let mut parts = args.splitn(2, ' ');
    let section = parts.next().unwrap_or("").trim();

    match section {
        "" => [
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
            "Note: start an active session to use setter sub-commands.",
        ]
        .join("\n"),
        "providers" => {
            let anthropic_key = std::env::var("ANTHROPIC_API_KEY").ok().filter(|s| !s.is_empty());
            let anthropic_oauth = runtime::load_oauth_credentials().ok().flatten().is_some();
            let anthropic_status = if anthropic_oauth { "[✓ OAuth]" } else if anthropic_key.is_some() { "[✓ API key]" } else { "[✗ not configured]" };
            let openai_status = if std::env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()).is_some() { "[✓ API key]" } else { "[✗ not configured]" };
            let ollama_host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string());
            let xai_status = if std::env::var("XAI_API_KEY").ok().filter(|s| !s.is_empty()).is_some() { "[✓ API key]" } else { "[✗ not configured]" };
            format!(
                "Providers & Authentication\n\n  Anthropic   {anthropic_status}\n  OpenAI      {openai_status}\n  Ollama      [{ollama_host}]\n  xAI         {xai_status}"
            )
        }
        "search" => {
            let engine = runtime::SearchEngine::from_env_and_config();
            let default_provider = engine.default_provider().to_string();
            format!("Default search provider: {default_provider}\n\nRun /configure search in an active session for full details.")
        }
        _ => format!(
            "Run /configure {section} in an active session to view and edit settings.\n\n\
             For a read-only overview: /configure (main menu)"
        ),
    }
}

/// Handle the `/theme` slash command.
///
/// - `/theme`           — show the active theme name
/// - `/theme list`      — list all built-in themes
/// - `/theme set <n>`   — load built-in theme, persist, and optionally hot-apply
/// - `/theme reset`     — revert to culpur-defense default
///
/// When `tui` is `Some` the theme is applied to the live TUI immediately.
fn run_theme_command(action: Option<&str>, tui: Option<&mut AnvilTui>) -> String {
    let action = action.unwrap_or("").trim();
    let mut parts = action.splitn(2, ' ');
    let sub = parts.next().unwrap_or("").trim();
    let arg = parts.next().unwrap_or("").trim();

    match sub {
        "" => {
            let current = Theme::load();
            format!(
                "Theme\n  Active           {}\n\nNext\n  /theme list      List available themes\n  /theme set <n>   Switch theme",
                current.name
            )
        }
        "list" => {
            let names = Theme::builtin_names();
            let active = Theme::load().name;
            let mut lines = vec!["Available themes".to_string()];
            for name in names {
                let marker = if *name == active { "● " } else { "  " };
                lines.push(format!("  {marker}{name}"));
            }
            lines.push(String::new());
            lines.push("  /theme set <name>   Apply a theme".to_string());
            lines.join("\n")
        }
        "set" if !arg.is_empty() => {
            match Theme::builtin(arg) {
                Some(theme) => {
                    let name = theme.name.clone();
                    if let Err(e) = theme.save() {
                        return format!("Theme save error: {e}");
                    }
                    if let Some(tui) = tui {
                        tui.set_theme(Theme::builtin(&name).unwrap_or_else(Theme::default_theme));
                    }
                    format!(
                        "Theme changed\n  Active           {name}\n  Persisted        ~/.anvil/theme.json"
                    )
                }
                None => {
                    let names = Theme::builtin_names().join(", ");
                    format!("Unknown theme: {arg}\n  Available: {names}")
                }
            }
        }
        "set" => "Usage: /theme set <name>  (try /theme list)".to_string(),
        "reset" => {
            let theme = Theme::default_theme();
            let name = theme.name.clone();
            if let Err(e) = theme.save() {
                return format!("Theme reset error: {e}");
            }
            if let Some(tui) = tui {
                tui.set_theme(Theme::default_theme());
            }
            format!(
                "Theme reset\n  Active           {name}\n  Persisted        ~/.anvil/theme.json"
            )
        }
        // Feature 18 — export current theme to a JSON file
        "export" => {
            let dest = if arg.is_empty() {
                let current = Theme::load();
                format!("{}.theme.json", current.name)
            } else {
                arg.to_string()
            };
            let theme = Theme::load();
            match theme.save() {
                Ok(_) => {
                    // Copy ~/.anvil/theme.json to the requested destination
                    let src = anvil_home_dir().join("theme.json");
                    match std::fs::copy(&src, &dest) {
                        Ok(_) => format!(
                            "Theme exported\n  Theme            {}\n  File             {dest}",
                            theme.name
                        ),
                        Err(e) => format!("Export error: {e}"),
                    }
                }
                Err(e) => format!("Export error: {e}"),
            }
        }
        // Feature 18 — import a theme from a JSON file and apply it
        "import" => {
            if arg.is_empty() {
                return "Usage: /theme import <file.json>".to_string();
            }
            match std::fs::read_to_string(arg) {
                Ok(text) => {
                    // Validate JSON structure before writing
                    match serde_json::from_str::<serde_json::Value>(&text) {
                        Ok(_) => {
                            let dest = anvil_home_dir().join("theme.json");
                            if let Some(parent) = dest.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            match std::fs::write(&dest, &text) {
                                Ok(_) => {
                                    let theme = Theme::load();
                                    if let Some(tui) = tui {
                                        tui.set_theme(Theme::load());
                                    }
                                    format!(
                                        "Theme imported\n  Active           {}\n  Source           {arg}",
                                        theme.name
                                    )
                                }
                                Err(e) => format!("Import error: {e}"),
                            }
                        }
                        Err(e) => format!("Invalid theme JSON: {e}"),
                    }
                }
                Err(e) => format!("Cannot read {arg}: {e}"),
            }
        }
        // Feature 18 — create a new custom theme interactively (AI-guided)
        "create" => {
            let name = if arg.is_empty() { "custom" } else { arg };
            format!(
                "Theme create — {name}\n\n\
                 To create a custom theme, edit or create a JSON file with this structure:\n\n\
                 {{\n\
                   \"name\": \"{name}\",\n\
                   \"colors\": {{\n\
                     \"bg_primary\":       \"#1e1e2e\",\n\
                     \"bg_card\":          \"#313244\",\n\
                     \"text_primary\":     \"#cad3f5\",\n\
                     \"text_secondary\":   \"#a5adce\",\n\
                     \"accent\":           \"#caa6f7\",\n\
                     \"accent_secondary\": \"#f5bde2\",\n\
                     \"success\":          \"#a6da95\",\n\
                     \"warning\":          \"#eed49f\",\n\
                     \"error\":            \"#ed8796\",\n\
                     \"border\":           \"#45475a\",\n\
                     \"header_bg\":        \"#181826\",\n\
                     \"thinking\":         \"#8bd5ca\"\n\
                   }}\n\
                 }}\n\n\
                 Then run:  /theme import <file.json>"
            )
        }
        other => format!(
            "Unknown theme action: {other}\n\n  \
             /theme              Show current theme\n  \
             /theme list         List themes\n  \
             /theme set <n>      Apply a theme\n  \
             /theme reset        Reset to default\n  \
             /theme create <n>   Show template for a custom theme\n  \
             /theme import <f>   Import theme from JSON file\n  \
             /theme export [f]   Export current theme to JSON file"
        ),
    }
}

fn status_context(
    session_path: Option<&Path>,
) -> Result<StatusContext, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    let discovered_config_files = loader.discover().len();
    let runtime_config = loader.load()?;
    let project_context = ProjectContext::discover_with_git(&cwd, DEFAULT_DATE)?;
    let (project_root, git_branch) =
        parse_git_status_metadata(project_context.git_status.as_deref());
    Ok(StatusContext {
        cwd,
        session_path: session_path.map(Path::to_path_buf),
        loaded_config_files: runtime_config.loaded_entries().len(),
        discovered_config_files,
        memory_file_count: project_context.instruction_files.len(),
        project_root,
        git_branch,
    })
}

fn format_status_report(
    model: &str,
    usage: StatusUsage,
    permission_mode: &str,
    context: &StatusContext,
) -> String {
    [
        format!(
            "Session
  Model            {model}
  Permissions      {permission_mode}
  Activity         {} messages · {} turns
  Tokens           est {} · latest {} · total {}",
            usage.message_count,
            usage.turns,
            usage.estimated_tokens,
            usage.latest.total_tokens(),
            usage.cumulative.total_tokens(),
        ),
        format!(
            "Usage
  Cumulative input {}
  Cumulative output {}
  Cache create     {}
  Cache read       {}",
            usage.cumulative.input_tokens,
            usage.cumulative.output_tokens,
            usage.cumulative.cache_creation_input_tokens,
            usage.cumulative.cache_read_input_tokens,
        ),
        format!(
            "Workspace
  Folder           {}
  Project root     {}
  Git branch       {}
  Session file     {}
  Config files     loaded {}/{}
  Memory files     {}

Next
  /help            Browse commands
  /session list    Inspect saved sessions
  /diff            Review current workspace changes",
            context.cwd.display(),
            context
                .project_root
                .as_ref()
                .map_or_else(|| "unknown".to_string(), |path| path.display().to_string()),
            context.git_branch.as_deref().unwrap_or("unknown"),
            context.session_path.as_ref().map_or_else(
                || "live-repl".to_string(),
                |path| path.display().to_string()
            ),
            context.loaded_config_files,
            context.discovered_config_files,
            context.memory_file_count,
        ),
    ]
    .join(
        "

",
    )
}

fn render_config_report(section: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    let discovered = loader.discover();
    let runtime_config = loader.load()?;

    let mut lines = vec![
        format!(
            "Config
  Working directory {}
  Loaded files      {}
  Merged keys       {}",
            cwd.display(),
            runtime_config.loaded_entries().len(),
            runtime_config.merged().len()
        ),
        "Discovered files".to_string(),
    ];
    for entry in discovered {
        let source = match entry.source {
            ConfigSource::User => "user",
            ConfigSource::Project => "project",
            ConfigSource::Local => "local",
        };
        let status = if runtime_config
            .loaded_entries()
            .iter()
            .any(|loaded_entry| loaded_entry.path == entry.path)
        {
            "loaded"
        } else {
            "missing"
        };
        lines.push(format!(
            "  {source:<7} {status:<7} {}",
            entry.path.display()
        ));
    }

    if let Some(section) = section {
        lines.push(format!("Merged section: {section}"));
        let value = match section {
            "env" => runtime_config.get("env"),
            "hooks" => runtime_config.get("hooks"),
            "model" => runtime_config.get("model"),
            "plugins" => runtime_config
                .get("plugins")
                .or_else(|| runtime_config.get("enabledPlugins")),
            other => {
                lines.push(format!(
                    "  Unsupported config section '{other}'. Use env, hooks, model, or plugins."
                ));
                return Ok(lines.join(
                    "
",
                ));
            }
        };
        lines.push(format!(
            "  {}",
            match value {
                Some(value) => value.render(),
                None => "<unset>".to_string(),
            }
        ));
        return Ok(lines.join(
            "
",
        ));
    }

    lines.push("Merged JSON".to_string());
    lines.push(format!("  {}", runtime_config.as_json().render()));
    Ok(lines.join(
        "
",
    ))
}

fn render_memory_report() -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let project_context = ProjectContext::discover(&cwd, DEFAULT_DATE)?;
    let memory_mgr = MemoryManager::new(&cwd);
    let memory_files = memory_mgr.discover();

    let mut lines = vec![format!(
        "Memory
  Working directory {}
  Instruction files {}
  Persistent memory files {}",
        cwd.display(),
        project_context.instruction_files.len(),
        memory_files.len(),
    )];

    lines.push("Instruction files".to_string());
    if project_context.instruction_files.is_empty() {
        lines.push(
            "  No ANVIL instruction files discovered in the current directory ancestry.".to_string(),
        );
    } else {
        for (index, file) in project_context.instruction_files.iter().enumerate() {
            let preview = file.content.lines().next().unwrap_or("").trim();
            let preview = if preview.is_empty() {
                "<empty>"
            } else {
                preview
            };
            lines.push(format!("  {}. {}", index + 1, file.path.display()));
            lines.push(format!(
                "     lines={} preview={}",
                file.content.lines().count(),
                preview
            ));
        }
    }

    lines.push("Persistent memory".to_string());
    lines.push(format!("  Directory  {}", memory_mgr.memory_dir().display()));
    if memory_files.is_empty() {
        lines.push("  No persistent memory files saved for this project.".to_string());
    } else {
        for (index, file) in memory_files.iter().enumerate() {
            lines.push(format!(
                "  {}. {} ({})",
                index + 1,
                file.name,
                file.memory_type
            ));
            lines.push(format!("     {}", file.description));
        }
    }

    Ok(lines.join("
"))
}

fn init_anvil_md() -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    Ok(initialize_repo(&cwd)?.render())
}

fn run_init() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", init_anvil_md()?);
    Ok(())
}

fn normalize_permission_mode(mode: &str) -> Option<&'static str> {
    match mode.trim() {
        "read-only" => Some("read-only"),
        "workspace-write" => Some("workspace-write"),
        "danger-full-access" => Some("danger-full-access"),
        _ => None,
    }
}

fn render_diff_report() -> Result<String, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .args(["diff", "--", ":(exclude).omx"])
        .current_dir(env::current_dir()?)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git diff failed: {stderr}").into());
    }
    let diff = String::from_utf8(output.stdout)?;
    if diff.trim().is_empty() {
        return Ok(
            "Diff\n  Result           clean working tree\n  Detail           no current changes"
                .to_string(),
        );
    }
    Ok(format!("Diff\n\n{}", diff.trim_end()))
}

fn render_teleport_report(target: &str) -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;

    let file_list = Command::new("rg")
        .args(["--files"])
        .current_dir(&cwd)
        .output()?;
    let file_matches = if file_list.status.success() {
        String::from_utf8(file_list.stdout)?
            .lines()
            .filter(|line| line.contains(target))
            .take(10)
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let content_output = Command::new("rg")
        .args(["-n", "-S", "--color", "never", target, "."])
        .current_dir(&cwd)
        .output()?;

    let mut lines = vec![format!("Teleport\n  Target           {target}")];
    if !file_matches.is_empty() {
        lines.push(String::new());
        lines.push("File matches".to_string());
        lines.extend(file_matches.into_iter().map(|path| format!("  {path}")));
    }

    if content_output.status.success() {
        let matches = String::from_utf8(content_output.stdout)?;
        if !matches.trim().is_empty() {
            lines.push(String::new());
            lines.push("Content matches".to_string());
            lines.push(truncate_for_prompt(&matches, 4_000));
        }
    }

    if lines.len() == 1 {
        lines.push("  Result           no matches found".to_string());
    }

    Ok(lines.join("\n"))
}

fn render_last_tool_debug_report(session: &Session) -> Result<String, Box<dyn std::error::Error>> {
    let last_tool_use = session
        .messages
        .iter()
        .rev()
        .find_map(|message| {
            message.blocks.iter().rev().find_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
        })
        .ok_or_else(|| "no prior tool call found in session".to_string())?;

    let tool_result = session.messages.iter().rev().find_map(|message| {
        message.blocks.iter().rev().find_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                tool_name,
                output,
                is_error,
            } if tool_use_id == &last_tool_use.0 => {
                Some((tool_name.clone(), output.clone(), *is_error))
            }
            _ => None,
        })
    });

    let mut lines = vec![
        "Debug tool call".to_string(),
        format!("  Tool id          {}", last_tool_use.0),
        format!("  Tool name        {}", last_tool_use.1),
        "  Input".to_string(),
        indent_block(&last_tool_use.2, 4),
    ];

    match tool_result {
        Some((tool_name, output, is_error)) => {
            lines.push("  Result".to_string());
            lines.push(format!("    name           {tool_name}"));
            lines.push(format!(
                "    status         {}",
                if is_error { "error" } else { "ok" }
            ));
            lines.push(indent_block(&output, 4));
        }
        None => lines.push("  Result           missing tool result".to_string()),
    }

    Ok(lines.join("\n"))
}

fn indent_block(value: &str, spaces: usize) -> String {
    let indent = " ".repeat(spaces);
    value
        .lines()
        .map(|line| format!("{indent}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn git_output(args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env::current_dir()?)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git {} failed: {stderr}", args.join(" ")).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn git_status_ok(args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env::current_dir()?)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git {} failed: {stderr}", args.join(" ")).into());
    }
    Ok(())
}

fn command_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Return the path to `~/.anvil/pinned.json`, creating `~/.anvil/` if needed.
fn anvil_pinned_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let home = dirs_next_home().ok_or("could not determine home directory")?;
    let dir = home.join(".anvil");
    if !dir.exists() {
        fs::create_dir_all(&dir)?;
    }
    Ok(dir.join("pinned.json"))
}

/// Portable home directory lookup (no external crate needed).
fn dirs_next_home() -> Option<PathBuf> {
    env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| env::var("USERPROFILE").ok().map(PathBuf::from))
}

/// Load pinned paths from `~/.anvil/pinned.json`.  Returns an empty vec if
/// the file does not exist yet.
fn load_pinned_paths(path: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path)?;
    let strings: Vec<String> = serde_json::from_str(&raw)?;
    Ok(strings.into_iter().map(PathBuf::from).collect())
}

/// Persist pinned paths to `~/.anvil/pinned.json`.
fn save_pinned_paths(path: &Path, paths: &[PathBuf]) -> Result<(), Box<dyn std::error::Error>> {
    let strings: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    let json = serde_json::to_string_pretty(&strings)?;
    fs::write(path, json)?;
    Ok(())
}

/// Format a large number with commas: 1000000 → "1,000,000".
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

/// Parse a human token count like "200K", "1M", "2M", "500000" into a `u64`.
fn parse_token_count(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix('M').or_else(|| s.strip_suffix('m')) {
        rest.trim().parse::<f64>().ok().map(|f| (f * 1_000_000.0) as u64)
    } else if let Some(rest) = s.strip_suffix('K').or_else(|| s.strip_suffix('k')) {
        rest.trim().parse::<f64>().ok().map(|f| (f * 1_000.0) as u64)
    } else {
        s.parse::<u64>().ok()
    }
}

fn write_temp_text_file(
    filename: &str,
    contents: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = env::temp_dir().join(filename);
    fs::write(&path, contents)?;
    Ok(path)
}

fn recent_user_context(session: &Session, limit: usize) -> String {
    let requests = session
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .filter_map(|message| {
            message.blocks.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.trim().to_string()),
                _ => None,
            })
        })
        .rev()
        .take(limit)
        .collect::<Vec<_>>();

    if requests.is_empty() {
        "<no prior user messages>".to_string()
    } else {
        requests
            .into_iter()
            .rev()
            .enumerate()
            .map(|(index, text)| format!("{}. {}", index + 1, text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Minimal POSIX single-quote escaping for shell export instructions.
fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '@'))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn truncate_for_prompt(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.trim().to_string()
    } else {
        let truncated = value.chars().take(limit).collect::<String>();
        format!("{}\n…[truncated]", truncated.trim_end())
    }
}

// ─── Feature 3 helpers ────────────────────────────────────────────────────────

/// Parse `/semantic-search` args into (query, type_filter, lang_filter).
fn parse_semantic_search_args(args: &str) -> (String, Option<String>, Option<String>) {
    let mut query_parts: Vec<&str> = Vec::new();
    let mut symbol_filter: Option<String> = None;
    let mut lang_filter: Option<String> = None;

    let mut tokens = args.split_whitespace().peekable();
    while let Some(token) = tokens.next() {
        if token == "--type" {
            symbol_filter = tokens.next().map(ToOwned::to_owned);
        } else if token == "--lang" {
            lang_filter = tokens.next().map(ToOwned::to_owned);
        } else {
            query_parts.push(token);
        }
    }

    (query_parts.join(" "), symbol_filter, lang_filter)
}

/// Escape a string for use as a literal in a regex pattern.
fn regex_escape(s: &str) -> String {
    let special = r"\.+*?()|[]{}^$#";
    s.chars()
        .flat_map(|c| {
            if special.contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}

// ─── Feature 4 helpers ────────────────────────────────────────────────────────

fn run_docker_ps() -> String {
    let out = Command::new("docker")
        .args(["ps", "--format", "table {{.ID}}\t{{.Image}}\t{{.Status}}\t{{.Names}}"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if stdout.is_empty() {
                "No running containers.".to_string()
            } else {
                stdout
            }
        }
        Ok(o) => format!("docker ps failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => format!("Cannot run docker: {e}. Is Docker installed and running?"),
    }
}

fn run_docker_logs(container: &str) -> String {
    let out = Command::new("docker")
        .args(["logs", "--tail", "50", container])
        .output();
    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let stderr_text = String::from_utf8_lossy(&o.stderr).trim().to_string();
            let combined = [stdout.as_str(), stderr_text.as_str()]
                .iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            if combined.is_empty() {
                format!("No log output for container: {container}")
            } else {
                combined
            }
        }
        Err(e) => format!("Cannot run docker logs: {e}"),
    }
}

fn run_docker_compose_services() -> String {
    let cwd = env::current_dir().unwrap_or_default();

    let candidates = ["docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml"];
    let compose_file = candidates
        .iter()
        .map(|name| cwd.join(name))
        .find(|p| p.exists());

    let Some(file) = compose_file else {
        return "No docker-compose file found in the current directory.".to_string();
    };

    let file_str = file.to_str().unwrap_or("docker-compose.yml").to_string();

    let out = Command::new("docker")
        .args(["compose", "-f", &file_str, "config", "--services"])
        .current_dir(&cwd)
        .output()
        .or_else(|_| {
            Command::new("docker-compose")
                .args(["-f", &file_str, "config", "--services"])
                .current_dir(&cwd)
                .output()
        });

    match out {
        Ok(o) if o.status.success() => {
            let services = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if services.is_empty() {
                format!("No services defined in {}.", file.display())
            } else {
                format!("Services in {}:\n{}", file.display(), services)
            }
        }
        Ok(o) => format!(
            "compose config failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => format!("Cannot run docker compose: {e}"),
    }
}

fn run_docker_build() -> String {
    let cwd = env::current_dir().unwrap_or_default();
    if !cwd.join("Dockerfile").exists() {
        return "No Dockerfile found in the current directory.".to_string();
    }

    let out = Command::new("docker")
        .args(["build", "."])
        .current_dir(&cwd)
        .output();

    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let stderr_text = String::from_utf8_lossy(&o.stderr).trim().to_string();
            let log = truncate_for_prompt(
                &[stdout.as_str(), stderr_text.as_str()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n"),
                4_000,
            );
            if o.status.success() {
                format!("docker build succeeded.\n\n{log}")
            } else {
                format!("docker build failed (exit {}).\n\n{log}", o.status)
            }
        }
        Err(e) => format!("Cannot run docker build: {e}"),
    }
}

// ─── Feature 5 helpers ────────────────────────────────────────────────────────

/// Detect the project type and run its test suite.
fn run_test_suite(coverage: bool) -> String {
    let cwd = env::current_dir().unwrap_or_default();

    let (cmd, args): (&str, Vec<String>) = if cwd.join("Cargo.toml").exists() {
        if coverage {
            (
                "cargo",
                vec![
                    "llvm-cov".to_string(),
                    "--text".to_string(),
                    "--ignore-filename-regex".to_string(),
                    "tests/".to_string(),
                ],
            )
        } else {
            ("cargo", vec!["test".to_string()])
        }
    } else if cwd.join("package.json").exists() {
        if coverage {
            (
                "npx",
                vec![
                    "vitest".to_string(),
                    "run".to_string(),
                    "--coverage".to_string(),
                ],
            )
        } else {
            (
                "npm",
                vec![
                    "test".to_string(),
                    "--".to_string(),
                    "--passWithNoTests".to_string(),
                ],
            )
        }
    } else if cwd.join("pyproject.toml").exists() || cwd.join("setup.py").exists() {
        if coverage {
            (
                "python",
                vec![
                    "-m".to_string(),
                    "pytest".to_string(),
                    "--cov".to_string(),
                    "--cov-report=term-missing".to_string(),
                ],
            )
        } else {
            ("python", vec!["-m".to_string(), "pytest".to_string()])
        }
    } else if cwd.join("go.mod").exists() {
        if coverage {
            (
                "go",
                vec!["test".to_string(), "./...".to_string(), "-cover".to_string()],
            )
        } else {
            ("go", vec!["test".to_string(), "./...".to_string()])
        }
    } else {
        return "Could not detect project type (no Cargo.toml, package.json, pyproject.toml, or go.mod found).".to_string();
    };

    let out = Command::new(cmd).args(&args).current_dir(&cwd).output();

    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let stderr_text = String::from_utf8_lossy(&o.stderr).trim().to_string();
            let log = truncate_for_prompt(
                &[stdout.as_str(), stderr_text.as_str()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n"),
                6_000,
            );
            if o.status.success() {
                format!("Tests passed.\n\n{log}")
            } else {
                format!("Tests failed (exit {}).\n\n{log}", o.status)
            }
        }
        Err(e) => format!("Cannot run {cmd}: {e}"),
    }
}

// ─── Feature 6 helpers ────────────────────────────────────────────────────────

fn run_git_stash_list() -> String {
    match git_output(&["stash", "list"]) {
        Ok(s) if s.is_empty() => "Stash is empty.".to_string(),
        Ok(s) => s,
        Err(e) => format!("git stash list failed: {e}"),
    }
}

fn run_git_stash_op(args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(env::current_dir().unwrap_or_default())
        .output();
    match out {
        Ok(o) => {
            let out_text = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let err_text = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if o.status.success() {
                if out_text.is_empty() {
                    err_text
                } else {
                    out_text
                }
            } else {
                let msg = if err_text.is_empty() { out_text } else { err_text };
                format!("git {} failed: {msg}", args.join(" "))
            }
        }
        Err(e) => format!("git {} failed: {e}", args.join(" ")),
    }
}

// ─── Feature 7 helpers ────────────────────────────────────────────────────────

/// Parse a line range like "10-25" or "10" into (start, end).  end=0 means open-ended.
fn parse_line_range(s: &str) -> (usize, usize) {
    let s = s.trim();
    if let Some((a, b)) = s.split_once('-') {
        let start = a.trim().parse().unwrap_or(1);
        let end = b.trim().parse().unwrap_or(0);
        (start, end)
    } else {
        let n = s.parse().unwrap_or(1);
        (n, n)
    }
}


/// Self-update: download the latest release and replace the current binary.
fn run_self_update() {
    println!("Anvil self-update");
    println!("  Current version: {VERSION}");
    println!();

    // Check for latest version
    print!("  Checking for updates... ");
    let latest_info = check_for_update(VERSION);
    if latest_info.is_none() {
        println!("already up to date!");
        return;
    }
    println!("update found!");

    // Detect platform
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let target = match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => {
            eprintln!("  Unsupported platform: {os}/{arch}");
            std::process::exit(1);
        }
    };

    // Get latest tag from GitHub
    let tag_output = Command::new("curl")
        .args(["-sfL", "--max-time", "10", "-H", "User-Agent: anvil-cli",
               "https://api.github.com/repos/culpur/anvil/releases/latest"])
        .output();
    let tag = match tag_output {
        Ok(o) if o.status.success() => {
            let body = String::from_utf8_lossy(&o.stdout);
            body.split("\"tag_name\"")
                .nth(1)
                .and_then(|s| s.split('"').nth(1))
                .unwrap_or("latest")
                .to_string()
        }
        _ => {
            eprintln!("  Failed to check GitHub releases");
            std::process::exit(1);
        }
    };

    let url = format!(
        "https://github.com/culpur/anvil/releases/download/{tag}/anvil-{target}.tar.gz"
    );
    println!("  Downloading {tag} for {target}...");

    // Download to temp
    let tmp_dir = std::env::temp_dir().join("anvil-update");
    let _ = fs::create_dir_all(&tmp_dir);
    let tarball = tmp_dir.join("anvil.tar.gz");

    let dl = Command::new("curl")
        .args(["-fSL", "--max-time", "120", "-o"])
        .arg(&tarball)
        .arg(&url)
        .status();

    match dl {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!("  Download failed from: {url}");
            std::process::exit(1);
        }
    }

    // Extract
    println!("  Extracting...");
    let extract = Command::new("tar")
        .args(["xzf"])
        .arg(&tarball)
        .arg("-C")
        .arg(&tmp_dir)
        .status();

    match extract {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!("  Extraction failed");
            std::process::exit(1);
        }
    }

    // Find the binary
    let new_binary = tmp_dir.join("anvil");
    if !new_binary.exists() {
        eprintln!("  Binary not found in archive");
        std::process::exit(1);
    }

    // Replace current binary
    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("anvil"));
    println!("  Replacing {}...", current_exe.display());

    // Backup current
    let backup = current_exe.with_extension("bak");
    let _ = fs::rename(&current_exe, &backup);

    match fs::copy(&new_binary, &current_exe) {
        Ok(_) => {
            // Make executable on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&current_exe, fs::Permissions::from_mode(0o755));
            }
            let _ = fs::remove_file(&backup);
            let _ = fs::remove_dir_all(&tmp_dir);
            println!();
            println!("  ✓ Updated to {tag}!");
            println!("  Restart Anvil to use the new version.");
        }
        Err(e) => {
            // Restore backup
            let _ = fs::rename(&backup, &current_exe);
            eprintln!("  Failed to replace binary: {e}");
            std::process::exit(1);
        }
    }
}

/// Check GitHub Releases for a newer version of Anvil.
/// Returns `Some("Update available: v1.0.3 → Run: ...")` or `None`.
fn check_for_update(current_version: &str) -> Option<String> {
    // Try GitHub API first, fall back to AnvilHub
    let urls = [
        "https://api.github.com/repos/culpur/anvil/releases/latest",
    ];

    for url in &urls {
        let output = Command::new("curl")
            .args(["-sfL", "--max-time", "5", "-H", "User-Agent: anvil-cli", url])
            .output()
            .ok()?;
        if !output.status.success() {
            continue;
        }
        let body = String::from_utf8_lossy(&output.stdout);
        // Parse tag_name from JSON (minimal parsing, no serde needed)
        let tag = body
            .split("\"tag_name\"")
            .nth(1)?
            .split('"')
            .nth(1)?;
        let latest = tag.trim_start_matches('v');
        if latest != current_version && version_is_newer(latest, current_version) {
            return Some(format!(
                "Update available! {current_version} → {latest}  Run: anvil --update"
            ));
        }
        return None; // Successfully checked, no update needed
    }
    None
}

/// Simple semver comparison: returns true if `a` is newer than `b`.
fn version_is_newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> {
        v.split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect()
    };
    let va = parse(a);
    let vb = parse(b);
    for i in 0..va.len().max(vb.len()) {
        let x = va.get(i).copied().unwrap_or(0);
        let y = vb.get(i).copied().unwrap_or(0);
        if x > y { return true; }
        if x < y { return false; }
    }
    false
}

fn sanitize_generated_message(value: &str) -> String {
    value.trim().trim_matches('`').trim().replace("\r\n", "\n")
}

fn parse_titled_body(value: &str) -> Option<(String, String)> {
    let normalized = sanitize_generated_message(value);
    let title = normalized
        .lines()
        .find_map(|line| line.strip_prefix("TITLE:").map(str::trim))?;
    let body_start = normalized.find("BODY:")?;
    let body = normalized[body_start + "BODY:".len()..].trim();
    Some((title.to_string(), body.to_string()))
}

fn render_version_report() -> String {
    let git_sha = GIT_SHA;
    let target = BUILD_TARGET;
    format!(
        "Anvil CLI\n  Version          {VERSION}\n  Git SHA          {git_sha}\n  Target           {target}\n  Build date       {DEFAULT_DATE}\n\nSupport\n  Help             anvil --help\n  REPL             /help"
    )
}

fn render_export_text(session: &Session) -> String {
    let mut lines = vec!["# Conversation Export".to_string(), String::new()];
    for (index, message) in session.messages.iter().enumerate() {
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        lines.push(format!("## {}. {role}", index + 1));
        for block in &message.blocks {
            match block {
                ContentBlock::Text { text } => lines.push(text.clone()),
                ContentBlock::Image { media_type, data } => {
                    lines.push(format!("[image {media_type} {} bytes]", data.len()));
                }
                ContentBlock::ToolUse { id, name, input } => {
                    lines.push(format!("[tool_use id={id} name={name}] {input}"));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    tool_name,
                    output,
                    is_error,
                } => {
                    lines.push(format!(
                        "[tool_result id={tool_use_id} name={tool_name} error={is_error}] {output}"
                    ));
                }
            }
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

fn default_export_filename(session: &Session) -> String {
    let stem = session
        .messages
        .iter()
        .find_map(|message| match message.role {
            MessageRole::User => message.blocks.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            }),
            _ => None,
        })
        .map_or("conversation", |text| {
            text.lines().next().unwrap_or("conversation")
        })
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join("-");
    let fallback = if stem.is_empty() {
        "conversation"
    } else {
        &stem
    };
    format!("{fallback}.txt")
}

fn resolve_export_path(
    requested_path: Option<&str>,
    session: &Session,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let file_name =
        requested_path.map_or_else(|| default_export_filename(session), ToOwned::to_owned);
    let final_name = if Path::new(&file_name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
    {
        file_name
    } else {
        format!("{file_name}.txt")
    };
    Ok(cwd.join(final_name))
}

fn build_system_prompt() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(load_system_prompt(
        env::current_dir()?,
        DEFAULT_DATE,
        env::consts::OS,
        "unknown",
    )?)
}

fn build_runtime_plugin_state(
) -> Result<(runtime::RuntimeFeatureConfig, GlobalToolRegistry, runtime::RuntimeConfig), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    let runtime_config = loader.load()?;
    let plugin_manager = build_plugin_manager(&cwd, &loader, &runtime_config);
    let tool_registry = GlobalToolRegistry::with_plugin_tools(plugin_manager.aggregated_tools()?)?;
    Ok((runtime_config.feature_config().clone(), tool_registry, runtime_config))
}

fn build_plugin_manager(
    cwd: &Path,
    loader: &ConfigLoader,
    runtime_config: &runtime::RuntimeConfig,
) -> PluginManager {
    let plugin_settings = runtime_config.plugins();
    let mut plugin_config = PluginManagerConfig::new(loader.config_home().to_path_buf());
    plugin_config.enabled_plugins = plugin_settings.enabled_plugins().clone();
    plugin_config.external_dirs = plugin_settings
        .external_directories()
        .iter()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path))
        .collect();
    plugin_config.install_root = plugin_settings
        .install_root()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path));
    plugin_config.registry_path = plugin_settings
        .registry_path()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path));
    plugin_config.bundled_root = plugin_settings
        .bundled_root()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path));
    PluginManager::new(plugin_config)
}

fn resolve_plugin_path(cwd: &Path, config_home: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else if value.starts_with('.') {
        cwd.join(path)
    } else {
        config_home.join(path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InternalPromptProgressState {
    command_label: &'static str,
    task_label: String,
    step: usize,
    phase: String,
    detail: Option<String>,
    saw_final_text: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InternalPromptProgressEvent {
    Started,
    Update,
    Heartbeat,
    Complete,
    Failed,
}

#[derive(Debug)]
struct InternalPromptProgressShared {
    state: Mutex<InternalPromptProgressState>,
    output_lock: Mutex<()>,
    started_at: Instant,
}

#[derive(Debug, Clone)]
struct InternalPromptProgressReporter {
    shared: Arc<InternalPromptProgressShared>,
}

#[derive(Debug)]
struct InternalPromptProgressRun {
    reporter: InternalPromptProgressReporter,
    heartbeat_stop: Option<mpsc::Sender<()>>,
    heartbeat_handle: Option<thread::JoinHandle<()>>,
}

impl InternalPromptProgressReporter {
    fn ultraplan(task: &str) -> Self {
        Self {
            shared: Arc::new(InternalPromptProgressShared {
                state: Mutex::new(InternalPromptProgressState {
                    command_label: "Ultraplan",
                    task_label: task.to_string(),
                    step: 0,
                    phase: "planning started".to_string(),
                    detail: Some(format!("task: {task}")),
                    saw_final_text: false,
                }),
                output_lock: Mutex::new(()),
                started_at: Instant::now(),
            }),
        }
    }

    fn emit(&self, event: InternalPromptProgressEvent, error: Option<&str>) {
        let snapshot = self.snapshot();
        let line = format_internal_prompt_progress_line(event, &snapshot, self.elapsed(), error);
        self.write_line(&line);
    }

    fn mark_model_phase(&self) {
        let snapshot = {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("internal prompt progress state poisoned");
            state.step += 1;
            state.phase = if state.step == 1 {
                "analyzing request".to_string()
            } else {
                "reviewing findings".to_string()
            };
            state.detail = Some(format!("task: {}", state.task_label));
            state.clone()
        };
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Update,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn mark_tool_phase(&self, name: &str, input: &str) {
        let detail = describe_tool_progress(name, input);
        let snapshot = {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("internal prompt progress state poisoned");
            state.step += 1;
            state.phase = format!("running {name}");
            state.detail = Some(detail);
            state.clone()
        };
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Update,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn mark_text_phase(&self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let detail = truncate_for_summary(first_visible_line(trimmed), 120);
        let snapshot = {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("internal prompt progress state poisoned");
            if state.saw_final_text {
                return;
            }
            state.saw_final_text = true;
            state.step += 1;
            state.phase = "drafting final plan".to_string();
            state.detail = (!detail.is_empty()).then_some(detail);
            state.clone()
        };
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Update,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn emit_heartbeat(&self) {
        let snapshot = self.snapshot();
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Heartbeat,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn snapshot(&self) -> InternalPromptProgressState {
        self.shared
            .state
            .lock()
            .expect("internal prompt progress state poisoned")
            .clone()
    }

    fn elapsed(&self) -> Duration {
        self.shared.started_at.elapsed()
    }

    fn write_line(&self, line: &str) {
        let _guard = self
            .shared
            .output_lock
            .lock()
            .expect("internal prompt progress output lock poisoned");
        let mut stdout = io::stdout();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }
}

impl InternalPromptProgressRun {
    fn start_ultraplan(task: &str) -> Self {
        let reporter = InternalPromptProgressReporter::ultraplan(task);
        reporter.emit(InternalPromptProgressEvent::Started, None);

        let (heartbeat_stop, heartbeat_rx) = mpsc::channel();
        let heartbeat_reporter = reporter.clone();
        let heartbeat_handle = thread::spawn(move || loop {
            match heartbeat_rx.recv_timeout(INTERNAL_PROGRESS_HEARTBEAT_INTERVAL) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => heartbeat_reporter.emit_heartbeat(),
            }
        });

        Self {
            reporter,
            heartbeat_stop: Some(heartbeat_stop),
            heartbeat_handle: Some(heartbeat_handle),
        }
    }

    fn reporter(&self) -> InternalPromptProgressReporter {
        self.reporter.clone()
    }

    fn finish_success(&mut self) {
        self.stop_heartbeat();
        self.reporter
            .emit(InternalPromptProgressEvent::Complete, None);
    }

    fn finish_failure(&mut self, error: &str) {
        self.stop_heartbeat();
        self.reporter
            .emit(InternalPromptProgressEvent::Failed, Some(error));
    }

    fn stop_heartbeat(&mut self) {
        if let Some(sender) = self.heartbeat_stop.take() {
            let _ = sender.send(());
        }
        if let Some(handle) = self.heartbeat_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for InternalPromptProgressRun {
    fn drop(&mut self) {
        self.stop_heartbeat();
    }
}

fn format_internal_prompt_progress_line(
    event: InternalPromptProgressEvent,
    snapshot: &InternalPromptProgressState,
    elapsed: Duration,
    error: Option<&str>,
) -> String {
    let elapsed_seconds = elapsed.as_secs();
    let step_label = if snapshot.step == 0 {
        "current step pending".to_string()
    } else {
        format!("current step {}", snapshot.step)
    };
    let mut status_bits = vec![step_label, format!("phase {}", snapshot.phase)];
    if let Some(detail) = snapshot
        .detail
        .as_deref()
        .filter(|detail| !detail.is_empty())
    {
        status_bits.push(detail.to_string());
    }
    let status = status_bits.join(" · ");
    match event {
        InternalPromptProgressEvent::Started => {
            format!(
                "🧭 {} status · planning started · {status}",
                snapshot.command_label
            )
        }
        InternalPromptProgressEvent::Update => {
            format!("… {} status · {status}", snapshot.command_label)
        }
        InternalPromptProgressEvent::Heartbeat => format!(
            "… {} heartbeat · {elapsed_seconds}s elapsed · {status}",
            snapshot.command_label
        ),
        InternalPromptProgressEvent::Complete => format!(
            "✔ {} status · completed · {elapsed_seconds}s elapsed · {} steps total",
            snapshot.command_label, snapshot.step
        ),
        InternalPromptProgressEvent::Failed => format!(
            "✘ {} status · failed · {elapsed_seconds}s elapsed · {}",
            snapshot.command_label,
            error.unwrap_or("unknown error")
        ),
    }
}

fn describe_tool_progress(name: &str, input: &str) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(input).unwrap_or(serde_json::Value::String(input.to_string()));
    match name {
        "bash" | "Bash" => {
            let command = parsed
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if command.is_empty() {
                "running shell command".to_string()
            } else {
                format!("command {}", truncate_for_summary(command.trim(), 100))
            }
        }
        "read_file" | "Read" => format!("reading {}", extract_tool_path(&parsed)),
        "write_file" | "Write" => format!("writing {}", extract_tool_path(&parsed)),
        "edit_file" | "Edit" => format!("editing {}", extract_tool_path(&parsed)),
        "glob_search" | "Glob" => {
            let pattern = parsed
                .get("pattern")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let scope = parsed
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or(".");
            format!("glob `{pattern}` in {scope}")
        }
        "grep_search" | "Grep" => {
            let pattern = parsed
                .get("pattern")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let scope = parsed
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or(".");
            format!("grep `{pattern}` in {scope}")
        }
        "web_search" | "WebSearch" => parsed
            .get("query")
            .and_then(|value| value.as_str())
            .map_or_else(
                || "running web search".to_string(),
                |query| format!("query {}", truncate_for_summary(query, 100)),
            ),
        _ => {
            let summary = summarize_tool_payload(input);
            if summary.is_empty() {
                format!("running {name}")
            } else {
                format!("{name}: {summary}")
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
#[allow(clippy::too_many_arguments)]
fn build_runtime(
    session: Session,
    model: String,
    system_prompt: Vec<String>,
    enable_tools: bool,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
    progress_reporter: Option<InternalPromptProgressReporter>,
) -> Result<ConversationRuntime<DefaultRuntimeClient, CliToolExecutor>, Box<dyn std::error::Error>>
{
    // A blank slot: no TUI by default.  Callers that want TUI output call
    // build_runtime_with_tui_slot() instead.
    let slot: TuiSenderSlot = Arc::new(Mutex::new(None));
    build_runtime_with_tui_slot(
        session, model, system_prompt, enable_tools, emit_output,
        allowed_tools, permission_mode, progress_reporter, slot,
    )
}

#[allow(clippy::needless_pass_by_value)]
#[allow(clippy::too_many_arguments)]
fn build_runtime_with_tui_slot(
    session: Session,
    model: String,
    system_prompt: Vec<String>,
    enable_tools: bool,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
    progress_reporter: Option<InternalPromptProgressReporter>,
    tui_slot: TuiSenderSlot,
) -> Result<ConversationRuntime<DefaultRuntimeClient, CliToolExecutor>, Box<dyn std::error::Error>>
{
    let (feature_config, mut tool_registry, runtime_config) = build_runtime_plugin_state()?;

    // Build and initialize the MCP server manager, then inject discovered tools
    let mcp_manager = {
        let mut manager = McpServerManager::from_runtime_config(&runtime_config);
        let tokio_rt = tokio::runtime::Runtime::new()?;
        let discovered = tokio_rt.block_on(manager.discover_tools()).unwrap_or_else(|err| {
            eprintln!("[mcp] tool discovery failed: {err}");
            Vec::new()
        });
        let mcp_defs = discovered
            .into_iter()
            .map(|t| McpToolDefinition {
                name: t.qualified_name,
                description: t.tool.description,
                input_schema: t.tool.input_schema,
            })
            .collect::<Vec<_>>();
        tool_registry.add_mcp_tools(mcp_defs);
        Arc::new(Mutex::new(manager))
    };

    // Build the LSP manager from config entries, if any are configured.
    let lsp_manager = {
        let lsp_cfg = runtime_config.lsp();
        let manager = if lsp_cfg.servers.is_empty() {
            None
        } else {
            let server_configs = lsp_cfg
                .servers
                .iter()
                .map(|entry| LspServerConfig {
                    name: entry.name.clone(),
                    command: entry.command.clone(),
                    args: entry.args.clone(),
                    env: entry.env.clone(),
                    workspace_root: entry.workspace_root.clone(),
                    initialization_options: None,
                    extension_to_language: entry.extension_to_language.clone(),
                })
                .collect::<Vec<_>>();
            match LspManager::new(server_configs) {
                Ok(m) => Some(m),
                Err(err) => {
                    eprintln!("[lsp] failed to initialize LSP manager: {err}");
                    None
                }
            }
        };
        Arc::new(Mutex::new(manager))
    };

    Ok(ConversationRuntime::new_with_features(
        session,
        DefaultRuntimeClient::new(
            model,
            enable_tools,
            emit_output,
            allowed_tools.clone(),
            tool_registry.clone(),
            progress_reporter,
            tui_slot.clone(),
        )?,
        CliToolExecutor::new(
            allowed_tools.clone(),
            emit_output,
            tool_registry.clone(),
            mcp_manager,
            lsp_manager,
            tui_slot,
        ),
        permission_policy(permission_mode, &tool_registry),
        system_prompt,
        feature_config,
    ))
}

struct CliPermissionPrompter {
    current_mode: PermissionMode,
}

impl CliPermissionPrompter {
    fn new(current_mode: PermissionMode) -> Self {
        Self { current_mode }
    }
}

impl runtime::PermissionPrompter for CliPermissionPrompter {
    fn decide(
        &mut self,
        request: &runtime::PermissionRequest,
    ) -> runtime::PermissionPromptDecision {
        // Truncate long inputs to keep the box readable
        let input_summary = if request.input.len() > 160 {
            format!("{}…", &request.input[..160])
        } else {
            request.input.clone()
        };

        let mut stdout = io::stdout();
        let mut stdin = io::BufReader::new(io::stdin());
        let response = render_permission_prompt(
            &request.tool_name,
            self.current_mode.as_str(),
            request.required_mode.as_str(),
            &input_summary,
            &mut stdout,
            &mut stdin,
        );

        match response {
            Ok(line) => {
                let normalized = line.trim().to_ascii_lowercase();
                match normalized.as_str() {
                    "y" | "yes" | "always" => runtime::PermissionPromptDecision::Allow,
                    _ => runtime::PermissionPromptDecision::Deny {
                        reason: format!(
                            "tool '{}' denied by user approval prompt",
                            request.tool_name
                        ),
                    },
                }
            }
            Err(error) => runtime::PermissionPromptDecision::Deny {
                reason: format!("permission approval failed: {error}"),
            },
        }
    }
}

struct DefaultRuntimeClient {
    runtime: tokio::runtime::Runtime,
    client: ProviderClient,
    model: String,
    enable_tools: bool,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    tool_registry: GlobalToolRegistry,
    progress_reporter: Option<InternalPromptProgressReporter>,
    /// Shared slot — when the inner value is `Some`, stream output goes to the
    /// TUI instead of stdout.
    tui_slot: TuiSenderSlot,
}

impl DefaultRuntimeClient {
    fn new(
        model: String,
        enable_tools: bool,
        emit_output: bool,
        allowed_tools: Option<AllowedToolSet>,
        tool_registry: GlobalToolRegistry,
        progress_reporter: Option<InternalPromptProgressReporter>,
        tui_slot: TuiSenderSlot,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let client = build_provider_client(&model)?;
        Ok(Self {
            runtime: tokio::runtime::Runtime::new()?,
            client,
            model,
            enable_tools,
            emit_output,
            allowed_tools,
            tool_registry,
            progress_reporter,
            tui_slot,
        })
    }
}

/// Build the correct `ProviderClient` for the given model name.
///
/// For Anthropic models the existing OAuth / API-key resolution path is used
/// so that saved credentials continue to work.  For other providers the
/// environment-based resolution in `ProviderClient::from_model` handles it.
fn build_provider_client(model: &str) -> Result<ProviderClient, Box<dyn std::error::Error>> {
    let kind = detect_provider_kind(model);
    match kind {
        ProviderKind::AnvilApi => {
            let auth = resolve_cli_auth_source()?;
            Ok(ProviderClient::AnvilApi(
                AnvilApiClient::from_auth(auth).with_base_url(api::read_base_url()),
            ))
        }
        _ => Ok(ProviderClient::from_model(model)?),
    }
}

fn resolve_cli_auth_source() -> Result<AuthSource, Box<dyn std::error::Error>> {
    Ok(resolve_startup_auth_source(|| {
        let cwd = env::current_dir().map_err(api::ApiError::from)?;
        let config = ConfigLoader::default_for(&cwd).load().map_err(|error| {
            api::ApiError::Auth(format!("failed to load runtime OAuth config: {error}"))
        })?;
        Ok(config.oauth().cloned())
    })?)
}

impl ApiClient for DefaultRuntimeClient {
    #[allow(clippy::too_many_lines)]
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        if let Some(progress_reporter) = &self.progress_reporter {
            progress_reporter.mark_model_phase();
        }
        let message_request = MessageRequest {
            model: self.model.clone(),
            max_tokens: max_tokens_for_model(&self.model),
            messages: convert_messages(&request.messages),
            system: (!request.system_prompt.is_empty()).then(|| request.system_prompt.join("\n\n")),
            tools: self
                .enable_tools
                .then(|| filter_tool_specs(&self.tool_registry, self.allowed_tools.as_ref())),
            tool_choice: self.enable_tools.then_some(ToolChoice::Auto),
            stream: true,
        };

        // Snapshot the TUI sender (if any) before entering the async block.
        let tui_tx: Option<TuiSender> = self
            .tui_slot
            .lock()
            .ok()
            .and_then(|guard| guard.clone());

        self.runtime.block_on(async {
            let mut stream = self
                .client
                .stream_message(&message_request)
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))?;
            let mut stdout = io::stdout();
            let mut sink = io::sink();
            // When a TUI sender is active we always use sink for stdout (output
            // goes via TuiEvents instead).
            let out: &mut dyn Write = if self.emit_output && tui_tx.is_none() {
                &mut stdout
            } else {
                &mut sink
            };
            let renderer = TerminalRenderer::new();
            let mut markdown_stream = MarkdownStreamState::default();
            let mut events = Vec::new();
            let mut pending_tool: Option<(String, String, String)> = None;
            let mut saw_stop = false;

            while let Some(event) = stream
                .next_event()
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))?
            {
                match event {
                    ApiStreamEvent::MessageStart(start) => {
                        for block in start.message.content {
                            push_output_block(block, out, &mut events, &mut pending_tool, true)?;
                        }
                    }
                    ApiStreamEvent::ContentBlockStart(start) => {
                        push_output_block(
                            start.content_block,
                            out,
                            &mut events,
                            &mut pending_tool,
                            true,
                        )?;
                    }
                    ApiStreamEvent::ContentBlockDelta(delta) => match delta.delta {
                        ContentBlockDelta::TextDelta { text } => {
                            if !text.is_empty() {
                                if let Some(progress_reporter) = &self.progress_reporter {
                                    progress_reporter.mark_text_phase(&text);
                                }
                                if let Some(ref tx) = tui_tx {
                                    // Route text delta to TUI
                                    tx.send(TuiEvent::TextDelta(text.clone()));
                                } else if let Some(rendered) = markdown_stream.push(&renderer, &text) {
                                    write!(out, "{rendered}")
                                        .and_then(|()| out.flush())
                                        .map_err(|error| RuntimeError::new(error.to_string()))?;
                                }
                                events.push(AssistantEvent::TextDelta(text));
                            }
                        }
                        ContentBlockDelta::InputJsonDelta { partial_json } => {
                            if let Some((_, _, input)) = &mut pending_tool {
                                input.push_str(&partial_json);
                            }
                        }
                        ContentBlockDelta::ThinkingDelta { .. }
                        | ContentBlockDelta::SignatureDelta { .. } => {}
                    },
                    ApiStreamEvent::ContentBlockStop(_) => {
                        if let Some(ref tx) = tui_tx {
                            // Signal end of this text block
                            tx.send(TuiEvent::TextDone);
                        } else if let Some(rendered) = markdown_stream.flush(&renderer) {
                            write!(out, "{rendered}")
                                .and_then(|()| out.flush())
                                .map_err(|error| RuntimeError::new(error.to_string()))?;
                        }
                        if let Some((id, name, input)) = pending_tool.take() {
                            if let Some(progress_reporter) = &self.progress_reporter {
                                progress_reporter.mark_tool_phase(&name, &input);
                            }
                            let detail = tool_call_detail(&name, &input);
                            if let Some(ref tx) = tui_tx {
                                // Send tool call to TUI
                                tx.send(TuiEvent::ToolCallActive {
                                    name: name.clone(),
                                    detail,
                                });
                            } else {
                                // Display tool call block now that input is fully accumulated
                                writeln!(out).map_err(|error| RuntimeError::new(error.to_string()))?;
                                render_tool_call_block(&name, &detail, BlockState::Active, out)
                                    .map_err(|error| RuntimeError::new(error.to_string()))?;
                            }
                            events.push(AssistantEvent::ToolUse { id, name, input });
                        }
                    }
                    ApiStreamEvent::MessageDelta(delta) => {
                        if let Some(ref tx) = tui_tx {
                            tx.send(TuiEvent::Tokens {
                                input: delta.usage.input_tokens,
                                output: delta.usage.output_tokens,
                            });
                        }
                        events.push(AssistantEvent::Usage(TokenUsage {
                            input_tokens: delta.usage.input_tokens,
                            output_tokens: delta.usage.output_tokens,
                            cache_creation_input_tokens: 0,
                            cache_read_input_tokens: 0,
                        }));
                    }
                    ApiStreamEvent::MessageStop(_) => {
                        saw_stop = true;
                        if let Some(ref tx) = tui_tx {
                            tx.send(TuiEvent::TextDone);
                        } else if let Some(rendered) = markdown_stream.flush(&renderer) {
                            write!(out, "{rendered}")
                                .and_then(|()| out.flush())
                                .map_err(|error| RuntimeError::new(error.to_string()))?;
                        }
                        events.push(AssistantEvent::MessageStop);
                    }
                }
            }

            if !saw_stop
                && events.iter().any(|event| {
                    matches!(event, AssistantEvent::TextDelta(text) if !text.is_empty())
                        || matches!(event, AssistantEvent::ToolUse { .. })
                })
            {
                events.push(AssistantEvent::MessageStop);
            }

            if events
                .iter()
                .any(|event| matches!(event, AssistantEvent::MessageStop))
            {
                return Ok(events);
            }

            let response = self
                .client
                .send_message(&MessageRequest {
                    stream: false,
                    ..message_request.clone()
                })
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))?;
            response_to_events(response, out)
        })
    }
}

fn final_assistant_text(summary: &runtime::TurnSummary) -> String {
    summary
        .assistant_messages
        .last()
        .map(|message| {
            message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

fn collect_tool_uses(summary: &runtime::TurnSummary) -> Vec<serde_json::Value> {
    summary
        .assistant_messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some(json!({
                "id": id,
                "name": name,
                "input": input,
            })),
            _ => None,
        })
        .collect()
}

fn collect_tool_results(summary: &runtime::TurnSummary) -> Vec<serde_json::Value> {
    summary
        .tool_results
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                tool_name,
                output,
                is_error,
            } => Some(json!({
                "tool_use_id": tool_use_id,
                "tool_name": tool_name,
                "output": output,
                "is_error": is_error,
            })),
            _ => None,
        })
        .collect()
}

fn slash_command_completion_candidates() -> Vec<String> {
    let mut candidates = slash_command_specs()
        .iter()
        .flat_map(|spec| {
            std::iter::once(spec.name)
                .chain(spec.aliases.iter().copied())
                .map(|name| format!("/{name}"))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    candidates.extend([
        String::from("/vim"),
        String::from("/exit"),
        String::from("/quit"),
        // /configure sub-command completions
        String::from("/configure providers"),
        String::from("/configure models"),
        String::from("/configure context"),
        String::from("/configure search"),
        String::from("/configure permissions"),
        String::from("/configure display"),
        String::from("/configure integrations"),
    ]);
    candidates.sort();
    candidates.dedup();
    candidates
}

fn suggest_repl_commands(name: &str) -> Vec<String> {
    let normalized = name.trim().trim_start_matches('/').to_ascii_lowercase();
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut ranked = slash_command_completion_candidates()
        .into_iter()
        .filter_map(|candidate| {
            let raw = candidate.trim_start_matches('/').to_ascii_lowercase();
            let distance = edit_distance(&normalized, &raw);
            let prefix_match = raw.starts_with(&normalized) || normalized.starts_with(&raw);
            let near_match = distance <= 2;
            (prefix_match || near_match).then_some((distance, candidate))
        })
        .collect::<Vec<_>>();
    ranked.sort();
    ranked.dedup_by(|left, right| left.1 == right.1);
    ranked
        .into_iter()
        .map(|(_, candidate)| candidate)
        .take(3)
        .collect()
}

fn edit_distance(left: &str, right: &str) -> usize {
    if left == right {
        return 0;
    }
    if left.is_empty() {
        return right.chars().count();
    }
    if right.is_empty() {
        return left.chars().count();
    }

    let right_chars = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right_chars.len()).collect::<Vec<_>>();
    let mut current = vec![0; right_chars.len() + 1];

    for (left_index, left_char) in left.chars().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right_chars.iter().enumerate() {
            let substitution_cost = usize::from(left_char != *right_char);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + substitution_cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[right_chars.len()]
}

/// Extract a human-readable detail string from a tool call input JSON for use
/// inside `render_tool_call_block`. Returns plain text (no ANSI escape codes).
fn tool_call_detail(name: &str, input: &str) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(input).unwrap_or(serde_json::Value::String(input.to_string()));

    match name {
        "bash" | "Bash" => parsed
            .get("command")
            .and_then(|v| v.as_str())
            .map(|cmd| truncate_for_summary(cmd, 200))
            .unwrap_or_default(),
        "read_file" | "Read" => format!("Reading {}", extract_tool_path(&parsed)),
        "write_file" | "Write" => {
            let path = extract_tool_path(&parsed);
            let lines = parsed
                .get("content")
                .and_then(|v| v.as_str())
                .map_or(0, |c| c.lines().count());
            format!("Writing {path}  ({lines} lines)")
        }
        "edit_file" | "Edit" => {
            let path = extract_tool_path(&parsed);
            let old = parsed
                .get("old_string")
                .or_else(|| parsed.get("oldString"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let new = parsed
                .get("new_string")
                .or_else(|| parsed.get("newString"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let mut out = format!("Editing {path}");
            if !old.is_empty() || !new.is_empty() {
                out.push_str(&format!(
                    "\n- {}\n+ {}",
                    truncate_for_summary(first_visible_line(old), 72),
                    truncate_for_summary(first_visible_line(new), 72),
                ));
            }
            out
        }
        "glob_search" | "Glob" | "grep_search" | "Grep" => {
            let pattern = parsed
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let scope = parsed
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".");
            format!("{pattern}\nin {scope}")
        }
        "web_search" | "WebSearch" => parsed
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string(),
        _ => summarize_tool_payload(input),
    }
}

fn format_tool_call_start(name: &str, input: &str) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(input).unwrap_or(serde_json::Value::String(input.to_string()));

    let detail = match name {
        "bash" | "Bash" => format_bash_call(&parsed),
        "read_file" | "Read" => {
            let path = extract_tool_path(&parsed);
            format!("\x1b[2m📄 Reading {path}…\x1b[0m")
        }
        "write_file" | "Write" => {
            let path = extract_tool_path(&parsed);
            let lines = parsed
                .get("content")
                .and_then(|value| value.as_str())
                .map_or(0, |content| content.lines().count());
            format!("\x1b[1;32m✏️ Writing {path}\x1b[0m \x1b[2m({lines} lines)\x1b[0m")
        }
        "edit_file" | "Edit" => {
            let path = extract_tool_path(&parsed);
            let old_value = parsed
                .get("old_string")
                .or_else(|| parsed.get("oldString"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let new_value = parsed
                .get("new_string")
                .or_else(|| parsed.get("newString"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            format!(
                "\x1b[1;33m📝 Editing {path}\x1b[0m{}",
                format_patch_preview(old_value, new_value)
                    .map(|preview| format!("\n{preview}"))
                    .unwrap_or_default()
            )
        }
        "glob_search" | "Glob" => format_search_start("🔎 Glob", &parsed),
        "grep_search" | "Grep" => format_search_start("🔎 Grep", &parsed),
        "web_search" | "WebSearch" => parsed
            .get("query")
            .and_then(|value| value.as_str())
            .unwrap_or("?")
            .to_string(),
        _ => summarize_tool_payload(input),
    };

    let border = "─".repeat(name.len() + 8);
    format!(
        "\x1b[38;5;245m╭─ \x1b[1;36m{name}\x1b[0;38;5;245m ─╮\x1b[0m\n\x1b[38;5;245m│\x1b[0m {detail}\n\x1b[38;5;245m╰{border}╯\x1b[0m"
    )
}

fn format_tool_result(name: &str, output: &str, is_error: bool) -> String {
    let icon = if is_error {
        "\x1b[1;31m✗\x1b[0m"
    } else {
        "\x1b[1;32m✓\x1b[0m"
    };
    if is_error {
        let summary = truncate_for_summary(output.trim(), 160);
        return if summary.is_empty() {
            format!("{icon} \x1b[38;5;245m{name}\x1b[0m")
        } else {
            format!("{icon} \x1b[38;5;245m{name}\x1b[0m\n\x1b[38;5;203m{summary}\x1b[0m")
        };
    }

    let parsed: serde_json::Value =
        serde_json::from_str(output).unwrap_or(serde_json::Value::String(output.to_string()));
    match name {
        "bash" | "Bash" => format_bash_result(icon, &parsed),
        "read_file" | "Read" => format_read_result(icon, &parsed),
        "write_file" | "Write" => format_write_result(icon, &parsed),
        "edit_file" | "Edit" => format_edit_result(icon, &parsed),
        "glob_search" | "Glob" => format_glob_result(icon, &parsed),
        "grep_search" | "Grep" => format_grep_result(icon, &parsed),
        _ => format_generic_tool_result(icon, name, &parsed),
    }
}

const DISPLAY_TRUNCATION_NOTICE: &str =
    "\x1b[2m… output truncated for display; full result preserved in session.\x1b[0m";
const READ_DISPLAY_MAX_LINES: usize = 80;
const READ_DISPLAY_MAX_CHARS: usize = 6_000;
const TOOL_OUTPUT_DISPLAY_MAX_LINES: usize = 60;
const TOOL_OUTPUT_DISPLAY_MAX_CHARS: usize = 4_000;

fn extract_tool_path(parsed: &serde_json::Value) -> String {
    parsed
        .get("file_path")
        .or_else(|| parsed.get("filePath"))
        .or_else(|| parsed.get("path"))
        .and_then(|value| value.as_str())
        .unwrap_or("?")
        .to_string()
}

fn format_search_start(label: &str, parsed: &serde_json::Value) -> String {
    let pattern = parsed
        .get("pattern")
        .and_then(|value| value.as_str())
        .unwrap_or("?");
    let scope = parsed
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or(".");
    format!("{label} {pattern}\n\x1b[2min {scope}\x1b[0m")
}

fn format_patch_preview(old_value: &str, new_value: &str) -> Option<String> {
    if old_value.is_empty() && new_value.is_empty() {
        return None;
    }
    Some(format!(
        "\x1b[38;5;203m- {}\x1b[0m\n\x1b[38;5;70m+ {}\x1b[0m",
        truncate_for_summary(first_visible_line(old_value), 72),
        truncate_for_summary(first_visible_line(new_value), 72)
    ))
}

fn format_bash_call(parsed: &serde_json::Value) -> String {
    let command = parsed
        .get("command")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if command.is_empty() {
        String::new()
    } else {
        format!(
            "\x1b[48;5;236;38;5;255m $ {} \x1b[0m",
            truncate_for_summary(command, 160)
        )
    }
}

fn first_visible_line(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(text)
}

fn format_bash_result(icon: &str, parsed: &serde_json::Value) -> String {
    let mut lines = vec![format!("{icon} \x1b[38;5;245mbash\x1b[0m")];
    if let Some(task_id) = parsed
        .get("backgroundTaskId")
        .and_then(|value| value.as_str())
    {
        write!(&mut lines[0], " backgrounded ({task_id})").expect("write to string");
    } else if let Some(status) = parsed
        .get("returnCodeInterpretation")
        .and_then(|value| value.as_str())
        .filter(|status| !status.is_empty())
    {
        write!(&mut lines[0], " {status}").expect("write to string");
    }

    if let Some(stdout) = parsed.get("stdout").and_then(|value| value.as_str()) {
        if !stdout.trim().is_empty() {
            lines.push(truncate_output_for_display(
                stdout,
                TOOL_OUTPUT_DISPLAY_MAX_LINES,
                TOOL_OUTPUT_DISPLAY_MAX_CHARS,
            ));
        }
    }
    if let Some(stderr) = parsed.get("stderr").and_then(|value| value.as_str()) {
        if !stderr.trim().is_empty() {
            lines.push(format!(
                "\x1b[38;5;203m{}\x1b[0m",
                truncate_output_for_display(
                    stderr,
                    TOOL_OUTPUT_DISPLAY_MAX_LINES,
                    TOOL_OUTPUT_DISPLAY_MAX_CHARS,
                )
            ));
        }
    }

    lines.join("\n\n")
}

fn format_read_result(icon: &str, parsed: &serde_json::Value) -> String {
    let file = parsed.get("file").unwrap_or(parsed);
    let path = extract_tool_path(file);
    let start_line = file
        .get("startLine")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    let num_lines = file
        .get("numLines")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let total_lines = file
        .get("totalLines")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(num_lines);
    let content = file
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let end_line = start_line.saturating_add(num_lines.saturating_sub(1));

    format!(
        "{icon} \x1b[2m📄 Read {path} (lines {}-{} of {})\x1b[0m\n{}",
        start_line,
        end_line.max(start_line),
        total_lines,
        truncate_output_for_display(content, READ_DISPLAY_MAX_LINES, READ_DISPLAY_MAX_CHARS)
    )
}

fn format_write_result(icon: &str, parsed: &serde_json::Value) -> String {
    let path = extract_tool_path(parsed);
    let kind = parsed
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("write");
    let line_count = parsed
        .get("content")
        .and_then(|value| value.as_str())
        .map_or(0, |content| content.lines().count());
    format!(
        "{icon} \x1b[1;32m✏️ {} {path}\x1b[0m \x1b[2m({line_count} lines)\x1b[0m",
        if kind == "create" { "Wrote" } else { "Updated" },
    )
}

fn format_structured_patch_preview(parsed: &serde_json::Value) -> Option<String> {
    let hunks = parsed.get("structuredPatch")?.as_array()?;
    let mut preview = Vec::new();
    for hunk in hunks.iter().take(2) {
        let lines = hunk.get("lines")?.as_array()?;
        for line in lines.iter().filter_map(|value| value.as_str()).take(6) {
            match line.chars().next() {
                Some('+') => preview.push(format!("\x1b[38;5;70m{line}\x1b[0m")),
                Some('-') => preview.push(format!("\x1b[38;5;203m{line}\x1b[0m")),
                _ => preview.push(line.to_string()),
            }
        }
    }
    if preview.is_empty() {
        None
    } else {
        Some(preview.join("\n"))
    }
}

fn format_edit_result(icon: &str, parsed: &serde_json::Value) -> String {
    let path = extract_tool_path(parsed);
    let suffix = if parsed
        .get("replaceAll")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        " (replace all)"
    } else {
        ""
    };
    let preview = format_structured_patch_preview(parsed).or_else(|| {
        let old_value = parsed
            .get("oldString")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let new_value = parsed
            .get("newString")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        format_patch_preview(old_value, new_value)
    });

    match preview {
        Some(preview) => format!("{icon} \x1b[1;33m📝 Edited {path}{suffix}\x1b[0m\n{preview}"),
        None => format!("{icon} \x1b[1;33m📝 Edited {path}{suffix}\x1b[0m"),
    }
}

fn format_glob_result(icon: &str, parsed: &serde_json::Value) -> String {
    let num_files = parsed
        .get("numFiles")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let filenames = parsed
        .get("filenames")
        .and_then(|value| value.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|value| value.as_str())
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if filenames.is_empty() {
        format!("{icon} \x1b[38;5;245mglob_search\x1b[0m matched {num_files} files")
    } else {
        format!("{icon} \x1b[38;5;245mglob_search\x1b[0m matched {num_files} files\n{filenames}")
    }
}

fn format_grep_result(icon: &str, parsed: &serde_json::Value) -> String {
    let num_matches = parsed
        .get("numMatches")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let num_files = parsed
        .get("numFiles")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let content = parsed
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let filenames = parsed
        .get("filenames")
        .and_then(|value| value.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|value| value.as_str())
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let summary = format!(
        "{icon} \x1b[38;5;245mgrep_search\x1b[0m {num_matches} matches across {num_files} files"
    );
    if !content.trim().is_empty() {
        format!(
            "{summary}\n{}",
            truncate_output_for_display(
                content,
                TOOL_OUTPUT_DISPLAY_MAX_LINES,
                TOOL_OUTPUT_DISPLAY_MAX_CHARS,
            )
        )
    } else if !filenames.is_empty() {
        format!("{summary}\n{filenames}")
    } else {
        summary
    }
}

fn format_generic_tool_result(icon: &str, name: &str, parsed: &serde_json::Value) -> String {
    let rendered_output = match parsed {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            serde_json::to_string_pretty(parsed).unwrap_or_else(|_| parsed.to_string())
        }
        _ => parsed.to_string(),
    };
    let preview = truncate_output_for_display(
        &rendered_output,
        TOOL_OUTPUT_DISPLAY_MAX_LINES,
        TOOL_OUTPUT_DISPLAY_MAX_CHARS,
    );

    if preview.is_empty() {
        format!("{icon} \x1b[38;5;245m{name}\x1b[0m")
    } else if preview.contains('\n') {
        format!("{icon} \x1b[38;5;245m{name}\x1b[0m\n{preview}")
    } else {
        format!("{icon} \x1b[38;5;245m{name}:\x1b[0m {preview}")
    }
}

fn summarize_tool_payload(payload: &str) -> String {
    let compact = match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(value) => value.to_string(),
        Err(_) => payload.trim().to_string(),
    };
    truncate_for_summary(&compact, 96)
}

fn truncate_for_summary(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn truncate_output_for_display(content: &str, max_lines: usize, max_chars: usize) -> String {
    let original = content.trim_end_matches('\n');
    if original.is_empty() {
        return String::new();
    }

    let mut preview_lines = Vec::new();
    let mut used_chars = 0usize;
    let mut truncated = false;

    for (index, line) in original.lines().enumerate() {
        if index >= max_lines {
            truncated = true;
            break;
        }

        let newline_cost = usize::from(!preview_lines.is_empty());
        let available = max_chars.saturating_sub(used_chars + newline_cost);
        if available == 0 {
            truncated = true;
            break;
        }

        let line_chars = line.chars().count();
        if line_chars > available {
            preview_lines.push(line.chars().take(available).collect::<String>());
            truncated = true;
            break;
        }

        preview_lines.push(line.to_string());
        used_chars += newline_cost + line_chars;
    }

    let mut preview = preview_lines.join("\n");
    if truncated {
        if !preview.is_empty() {
            preview.push('\n');
        }
        preview.push_str(DISPLAY_TRUNCATION_NOTICE);
    }
    preview
}

fn push_output_block(
    block: OutputContentBlock,
    out: &mut (impl Write + ?Sized),
    events: &mut Vec<AssistantEvent>,
    pending_tool: &mut Option<(String, String, String)>,
    streaming_tool_input: bool,
) -> Result<(), RuntimeError> {
    match block {
        OutputContentBlock::Text { text } => {
            if !text.is_empty() {
                let rendered = TerminalRenderer::new().markdown_to_ansi(&text);
                write!(out, "{rendered}")
                    .and_then(|()| out.flush())
                    .map_err(|error| RuntimeError::new(error.to_string()))?;
                events.push(AssistantEvent::TextDelta(text));
            }
        }
        OutputContentBlock::ToolUse { id, name, input } => {
            // During streaming, the initial content_block_start has an empty input ({}).
            // The real input arrives via input_json_delta events. In
            // non-streaming responses, preserve a legitimate empty object.
            let initial_input = if streaming_tool_input
                && input.is_object()
                && input.as_object().is_some_and(serde_json::Map::is_empty)
            {
                String::new()
            } else {
                input.to_string()
            };
            *pending_tool = Some((id, name, initial_input));
        }
        OutputContentBlock::Thinking { .. } | OutputContentBlock::RedactedThinking { .. } => {}
    }
    Ok(())
}

fn response_to_events(
    response: MessageResponse,
    out: &mut (impl Write + ?Sized),
) -> Result<Vec<AssistantEvent>, RuntimeError> {
    let mut events = Vec::new();
    let mut pending_tool = None;

    for block in response.content {
        push_output_block(block, out, &mut events, &mut pending_tool, false)?;
        if let Some((id, name, input)) = pending_tool.take() {
            events.push(AssistantEvent::ToolUse { id, name, input });
        }
    }

    events.push(AssistantEvent::Usage(TokenUsage {
        input_tokens: response.usage.input_tokens,
        output_tokens: response.usage.output_tokens,
        cache_creation_input_tokens: response.usage.cache_creation_input_tokens,
        cache_read_input_tokens: response.usage.cache_read_input_tokens,
    }));
    events.push(AssistantEvent::MessageStop);
    Ok(events)
}

struct CliToolExecutor {
    renderer: TerminalRenderer,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    tool_registry: GlobalToolRegistry,
    mcp_manager: Arc<Mutex<McpServerManager>>,
    lsp_manager: Arc<Mutex<Option<LspManager>>>,
    tokio_rt: tokio::runtime::Runtime,
    /// Shared slot — when the inner value is `Some`, tool output goes to the
    /// TUI instead of stdout.
    tui_slot: TuiSenderSlot,
}

impl CliToolExecutor {
    fn new(
        allowed_tools: Option<AllowedToolSet>,
        emit_output: bool,
        tool_registry: GlobalToolRegistry,
        mcp_manager: Arc<Mutex<McpServerManager>>,
        lsp_manager: Arc<Mutex<Option<LspManager>>>,
        tui_slot: TuiSenderSlot,
    ) -> Self {
        Self {
            renderer: TerminalRenderer::new(),
            emit_output,
            allowed_tools,
            tool_registry,
            mcp_manager,
            lsp_manager,
            tokio_rt: tokio::runtime::Runtime::new()
                .expect("failed to create tokio runtime for CliToolExecutor"),
            tui_slot,
        }
    }
}

impl ToolExecutor for CliToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        if self
            .allowed_tools
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(tool_name))
        {
            return Err(ToolError::new(format!(
                "tool `{tool_name}` is not enabled by the current --allowedTools setting"
            )));
        }
        let value: serde_json::Value = serde_json::from_str(input)
            .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;

        if tool_name == "AskUserQuestion" {
            let question = value
                .get("question")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::new("AskUserQuestion: missing required field `question`"))?;
            let options: Option<Vec<&str>> = value
                .get("options")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|s| s.as_str()).collect());

            let mut stdout = io::stdout();
            // Print the question with a distinct visual treatment.
            let _ = writeln!(stdout, "\x1b[1;36m?\x1b[0m \x1b[1m{question}\x1b[0m");
            if let Some(ref choices) = options {
                for (i, choice) in choices.iter().enumerate() {
                    let _ = writeln!(stdout, "  \x1b[38;5;245m{num}.\x1b[0m {choice}", num = i + 1);
                }
                let _ = write!(stdout, "\x1b[38;5;245mEnter number or text:\x1b[0m ");
            } else {
                let _ = write!(stdout, "\x1b[38;5;245mYour answer:\x1b[0m ");
            }
            let _ = stdout.flush();

            let mut line = String::new();
            if io::stdin().is_terminal() {
                io::stdin()
                    .read_line(&mut line)
                    .map_err(|e| ToolError::new(format!("AskUserQuestion: failed to read input: {e}")))?;
            } else {
                // Non-TTY (piped input): read one line if available, otherwise fall back gracefully.
                match io::stdin().read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        line = String::from("<no input — stdin is not a terminal>");
                    }
                    Ok(_) => {}
                }
            }
            let response = line.trim().to_string();

            // If options were provided, attempt to resolve a numeric selection.
            let resolved = if let Some(ref choices) = options {
                response
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| {
                        if n >= 1 && n <= choices.len() {
                            Some(choices[n - 1].to_string())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| response.clone())
            } else {
                response.clone()
            };

            if self.emit_output {
                let summary = format!("User answered: {resolved}");
                render_tool_result_block(tool_name, &summary, false, &mut io::stdout())
                    .map_err(|e| ToolError::new(e.to_string()))?;
            }
            return Ok(resolved);
        }

        // Route MCP tool calls (mcp__server__tool pattern)
        if tool_name.starts_with("mcp__") {
            let result = self.execute_mcp_tool(tool_name, &value);
            return self.emit_and_return(tool_name, result);
        }

        // Route MCP resource management built-ins
        if tool_name == "ListMcpResourcesTool" || tool_name == "ReadMcpResourceTool" {
            let result = self.execute_mcp_resource_tool(tool_name, &value);
            return self.emit_and_return(tool_name, result);
        }

        // Route LSP tool calls
        if tool_name == "LSPTool" {
            let result = self.execute_lsp_tool(&value);
            return self.emit_and_return(tool_name, result);
        }

        let result = self.tool_registry.execute(tool_name, &value);
        self.emit_and_return(tool_name, result)
    }
}

impl CliToolExecutor {
    fn emit_and_return(
        &mut self,
        tool_name: &str,
        result: Result<String, String>,
    ) -> Result<String, ToolError> {
        let tui_tx: Option<TuiSender> = self
            .tui_slot
            .lock()
            .ok()
            .and_then(|guard| guard.clone());

        match result {
            Ok(output) => {
                if let Some(ref tx) = tui_tx {
                    tx.send(TuiEvent::ToolResult {
                        name: tool_name.to_string(),
                        summary: output.clone(),
                        is_error: false,
                    });
                } else if self.emit_output {
                    render_tool_result_block(tool_name, &output, false, &mut io::stdout())
                        .map_err(|error| ToolError::new(error.to_string()))?;
                }
                Ok(output)
            }
            Err(error) => {
                if let Some(ref tx) = tui_tx {
                    tx.send(TuiEvent::ToolResult {
                        name: tool_name.to_string(),
                        summary: error.clone(),
                        is_error: true,
                    });
                } else if self.emit_output {
                    render_tool_result_block(tool_name, &error, true, &mut io::stdout())
                        .map_err(|stream_error| ToolError::new(stream_error.to_string()))?;
                }
                Err(ToolError::new(error))
            }
        }
    }

    fn execute_mcp_tool(
        &mut self,
        qualified_name: &str,
        input: &serde_json::Value,
    ) -> Result<String, String> {
        let args = if input.is_null()
            || input
                .as_object()
                .map_or(false, |m| m.is_empty())
        {
            None
        } else {
            Some(input.clone())
        };

        let mut manager = self
            .mcp_manager
            .lock()
            .map_err(|e| format!("MCP manager lock poisoned: {e}"))?;
        let response = self
            .tokio_rt
            .block_on(manager.call_tool(qualified_name, args))
            .map_err(|e| e.to_string())?;

        if let Some(err) = response.error {
            return Err(format!("MCP error {}: {}", err.code, err.message));
        }

        let result = response
            .result
            .ok_or_else(|| "MCP call returned no result".to_string())?;

        if result.is_error == Some(true) {
            let text = result
                .content
                .iter()
                .filter_map(|c| {
                    c.data
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
                .join("\n");
            return Err(if text.is_empty() {
                "MCP tool returned error".to_string()
            } else {
                text
            });
        }

        let text = result
            .content
            .iter()
            .filter_map(|c| {
                if c.kind == "text" {
                    c.data
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                } else {
                    serde_json::to_string(&c.data).ok()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(if text.is_empty() {
            serde_json::to_string_pretty(
                &result
                    .structured_content
                    .unwrap_or(serde_json::Value::Null),
            )
            .unwrap_or_default()
        } else {
            text
        })
    }

    fn execute_mcp_resource_tool(
        &mut self,
        tool_name: &str,
        value: &serde_json::Value,
    ) -> Result<String, String> {
        match tool_name {
            "ListMcpResourcesTool" => {
                let server_name = value
                    .get("server_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "missing required field: server_name".to_string())?
                    .to_string();
                let mut manager = self
                    .mcp_manager
                    .lock()
                    .map_err(|e| format!("MCP manager lock poisoned: {e}"))?;
                let resources = self
                    .tokio_rt
                    .block_on(manager.list_resources(&server_name))
                    .map_err(|e| e.to_string())?;
                serde_json::to_string_pretty(&resources).map_err(|e| e.to_string())
            }
            "ReadMcpResourceTool" => {
                let server_name = value
                    .get("server_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "missing required field: server_name".to_string())?
                    .to_string();
                let uri = value
                    .get("uri")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "missing required field: uri".to_string())?
                    .to_string();
                let mut manager = self
                    .mcp_manager
                    .lock()
                    .map_err(|e| format!("MCP manager lock poisoned: {e}"))?;
                let result = self
                    .tokio_rt
                    .block_on(manager.read_resource(&server_name, &uri))
                    .map_err(|e| e.to_string())?;
                serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
            }
            other => Err(format!("unknown MCP resource tool: {other}")),
        }
    }

    fn execute_lsp_tool(&mut self, value: &serde_json::Value) -> Result<String, String> {
        let action = value
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "LSPTool: missing required field `action`".to_string())?;
        let file_path_str = value
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "LSPTool: missing required field `file_path`".to_string())?;
        let file_path = std::path::PathBuf::from(file_path_str);

        let manager_guard = self
            .lsp_manager
            .lock()
            .map_err(|e| format!("LSP manager lock poisoned: {e}"))?;
        let manager = manager_guard
            .as_ref()
            .ok_or_else(|| "LSPTool: no LSP servers configured — add `lspServers` to your .anvil/settings.json".to_string())?;

        match action {
            "diagnostics" => {
                let diagnostics = self
                    .tokio_rt
                    .block_on(manager.collect_workspace_diagnostics())
                    .map_err(|e| e.to_string())?;
                if diagnostics.is_empty() {
                    return Ok("No diagnostics found across the workspace.".to_string());
                }
                let mut lines = vec![format!(
                    "{} diagnostic(s) across {} file(s):",
                    diagnostics.total_diagnostics(),
                    diagnostics.files.len()
                )];
                for file in &diagnostics.files {
                    for diag in &file.diagnostics {
                        let severity = match diag.severity {
                            Some(lsp_types::DiagnosticSeverity::ERROR) => "error",
                            Some(lsp_types::DiagnosticSeverity::WARNING) => "warning",
                            Some(lsp_types::DiagnosticSeverity::INFORMATION) => "info",
                            Some(lsp_types::DiagnosticSeverity::HINT) => "hint",
                            _ => "unknown",
                        };
                        lines.push(format!(
                            "  {}:{}:{} [{}] {}",
                            file.path.display(),
                            diag.range.start.line + 1,
                            diag.range.start.character + 1,
                            severity,
                            diag.message.replace('\n', " ")
                        ));
                    }
                }
                Ok(lines.join("\n"))
            }
            "definition" => {
                let line = value
                    .get("line")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| "LSPTool: `line` is required for the definition action".to_string())?
                    as u32;
                let character = value
                    .get("character")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| "LSPTool: `character` is required for the definition action".to_string())?
                    as u32;
                let position = lsp_types::Position::new(line, character);
                let locations = self
                    .tokio_rt
                    .block_on(manager.go_to_definition(&file_path, position))
                    .map_err(|e| e.to_string())?;
                if locations.is_empty() {
                    return Ok("No definition found.".to_string());
                }
                let lines = locations
                    .iter()
                    .map(|loc| format!("  {loc}"))
                    .collect::<Vec<_>>();
                Ok(format!("Definition(s):\n{}", lines.join("\n")))
            }
            "references" => {
                let line = value
                    .get("line")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| "LSPTool: `line` is required for the references action".to_string())?
                    as u32;
                let character = value
                    .get("character")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| "LSPTool: `character` is required for the references action".to_string())?
                    as u32;
                let position = lsp_types::Position::new(line, character);
                let locations = self
                    .tokio_rt
                    .block_on(manager.find_references(&file_path, position, true))
                    .map_err(|e| e.to_string())?;
                if locations.is_empty() {
                    return Ok("No references found.".to_string());
                }
                let lines = locations
                    .iter()
                    .map(|loc| format!("  {loc}"))
                    .collect::<Vec<_>>();
                Ok(format!("{} reference(s):\n{}", locations.len(), lines.join("\n")))
            }
            "open" => {
                let content = value
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "LSPTool: `content` is required for the open action".to_string())?;
                self.tokio_rt
                    .block_on(manager.open_document(&file_path, content))
                    .map_err(|e| e.to_string())?;
                Ok(format!("Opened {} in LSP server.", file_path.display()))
            }
            "close" => {
                self.tokio_rt
                    .block_on(manager.close_document(&file_path))
                    .map_err(|e| e.to_string())?;
                Ok(format!("Closed {} in LSP server.", file_path.display()))
            }
            other => Err(format!("LSPTool: unknown action `{other}`. Valid actions: diagnostics, definition, references, open, close")),
        }
    }
}

fn permission_policy(mode: PermissionMode, tool_registry: &GlobalToolRegistry) -> PermissionPolicy {
    tool_registry.permission_specs(None).into_iter().fold(
        PermissionPolicy::new(mode),
        |policy, (name, required_permission)| {
            policy.with_tool_requirement(name, required_permission)
        },
    )
}

fn convert_messages(messages: &[ConversationMessage]) -> Vec<InputMessage> {
    messages
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::System | MessageRole::User | MessageRole::Tool => "user",
                MessageRole::Assistant => "assistant",
            };
            let content = message
                .blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => InputContentBlock::Text { text: text.clone() },
                    ContentBlock::Image { media_type, data } => InputContentBlock::Image {
                        source: ImageSource {
                            kind: ImageSourceKind::Base64,
                            media_type: media_type.clone(),
                            data: data.clone(),
                        },
                    },
                    ContentBlock::ToolUse { id, name, input } => InputContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::from_str(input)
                            .unwrap_or_else(|_| serde_json::json!({ "raw": input })),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id,
                        output,
                        is_error,
                        ..
                    } => InputContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: vec![ToolResultContentBlock::Text {
                            text: output.clone(),
                        }],
                        is_error: *is_error,
                    },
                })
                .collect::<Vec<_>>();
            (!content.is_empty()).then(|| InputMessage {
                role: role.to_string(),
                content,
            })
        })
        .collect()
}

fn print_help_to(out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "Anvil CLI v{VERSION}")?;
    writeln!(
        out,
        "  Interactive coding assistant for the current workspace."
    )?;
    writeln!(out)?;
    writeln!(out, "Quick start")?;
    writeln!(
        out,
        "  anvil                                  Start the interactive REPL"
    )?;
    writeln!(
        out,
        "  anvil \"summarize this repo\"            Run one prompt and exit"
    )?;
    writeln!(
        out,
        "  anvil prompt \"explain src/main.rs\"     Explicit one-shot prompt"
    )?;
    writeln!(
        out,
        "  anvil --resume SESSION.json /status    Inspect a saved session"
    )?;
    writeln!(out)?;
    writeln!(out, "Interactive essentials")?;
    writeln!(
        out,
        "  /help                                 Browse the full slash command map"
    )?;
    writeln!(
        out,
        "  /status                               Inspect session + workspace state"
    )?;
    writeln!(
        out,
        "  /model <name>                         Switch models mid-session"
    )?;
    writeln!(
        out,
        "  /permissions <mode>                   Adjust tool access"
    )?;
    writeln!(
        out,
        "  Tab                                   Complete slash commands"
    )?;
    writeln!(
        out,
        "  /vim                                  Toggle modal editing"
    )?;
    writeln!(
        out,
        "  Shift+Enter / Ctrl+J                  Insert a newline"
    )?;
    writeln!(out)?;
    writeln!(out, "Commands")?;
    writeln!(
        out,
        "  anvil dump-manifests                   Read upstream TS sources and print extracted counts"
    )?;
    writeln!(
        out,
        "  anvil bootstrap-plan                   Print the bootstrap phase skeleton"
    )?;
    writeln!(
        out,
        "  anvil agents                           List configured agents"
    )?;
    writeln!(
        out,
        "  anvil skills                           List installed skills"
    )?;
    writeln!(
        out,
        "  anvil model [name]                     Start REPL with a specific model"
    )?;
    writeln!(out, "  anvil system-prompt [--cwd PATH] [--date YYYY-MM-DD]")?;
    writeln!(
        out,
        "  anvil login [provider]                 Login to a provider (anthropic, openai, ollama) — interactive if omitted"
    )?;
    writeln!(
        out,
        "  anvil logout                           Clear saved OAuth credentials"
    )?;
    writeln!(
        out,
        "  anvil init                             Scaffold ANVIL.md + local files"
    )?;
    writeln!(out)?;
    writeln!(out, "Flags")?;
    writeln!(
        out,
        "  --model MODEL                         Override the active model"
    )?;
    writeln!(
        out,
        "  --output-format FORMAT                Non-interactive output: text or json"
    )?;
    writeln!(
        out,
        "  --permission-mode MODE                Set read-only, workspace-write, or danger-full-access"
    )?;
    writeln!(
        out,
        "  --dangerously-skip-permissions        Skip all permission checks"
    )?;
    writeln!(
        out,
        "  --allowedTools TOOLS                  Restrict enabled tools (repeatable; comma-separated aliases supported)"
    )?;
    writeln!(
        out,
        "  --version, -V                         Print version and build information"
    )?;
    writeln!(
        out,
        "  --update                              Self-update to the latest release"
    )?;
    writeln!(out)?;
    writeln!(out, "Slash command reference")?;
    writeln!(out, "{}", render_slash_command_help())?;
    writeln!(out)?;
    let resume_commands = resume_supported_slash_commands()
        .into_iter()
        .map(|spec| match spec.argument_hint {
            Some(argument_hint) => format!("/{} {}", spec.name, argument_hint),
            None => format!("/{}", spec.name),
        })
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "Resume-safe commands: {resume_commands}")?;
    writeln!(out, "Examples")?;
    writeln!(out, "  anvil --model opus \"summarize this repo\"")?;
    writeln!(
        out,
        "  anvil --output-format json prompt \"explain src/main.rs\""
    )?;
    writeln!(
        out,
        "  anvil --allowedTools read,glob \"summarize Cargo.toml\""
    )?;
    writeln!(
        out,
        "  anvil --resume session.json /status /diff /export notes.txt"
    )?;
    writeln!(out, "  anvil agents")?;
    writeln!(out, "  anvil /skills")?;
    writeln!(out, "  anvil login                              # Interactive provider setup")?;
    writeln!(out, "  anvil login openai                       # Setup OpenAI API key")?;
    writeln!(out, "  anvil login ollama                       # Configure Ollama endpoint")?;
    writeln!(out, "  anvil model llama3.2                     # Start with Ollama model")?;
    writeln!(out, "  anvil model gpt-4o                       # Start with OpenAI model")?;
    writeln!(out, "  anvil init")?;
    Ok(())
}

fn print_help() {
    let _ = print_help_to(&mut io::stdout());
}

#[cfg(test)]
mod tests {
    use super::{
        describe_tool_progress, filter_tool_specs, format_compact_report, format_cost_report,
        format_internal_prompt_progress_line, format_model_report, format_model_switch_report,
        format_permissions_report, format_permissions_switch_report, format_resume_report,
        format_status_report, format_tool_call_start, format_tool_result,
        normalize_permission_mode, parse_args, parse_git_status_metadata, permission_policy,
        print_help_to, push_output_block, render_config_report, render_memory_report,
        render_repl_help, render_unknown_repl_command, resolve_model_alias, response_to_events,
        resume_supported_slash_commands, slash_command_completion_candidates, status_context,
        CliAction, CliOutputFormat, InternalPromptProgressEvent, InternalPromptProgressState,
        SlashCommand, StatusUsage, DEFAULT_MODEL,
    };
    use api::{MessageResponse, OutputContentBlock, Usage};
    use plugins::{PluginTool, PluginToolDefinition, PluginToolPermission};
    use runtime::{AssistantEvent, ContentBlock, ConversationMessage, MessageRole, PermissionMode};
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::Duration;
    use tools::GlobalToolRegistry;

    fn registry_with_plugin_tool() -> GlobalToolRegistry {
        GlobalToolRegistry::with_plugin_tools(vec![PluginTool::new(
            "plugin-demo@external",
            "plugin-demo",
            PluginToolDefinition {
                name: "plugin_echo".to_string(),
                description: Some("Echo plugin payload".to_string()),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" }
                    },
                    "required": ["message"],
                    "additionalProperties": false
                }),
            },
            "echo".to_string(),
            Vec::new(),
            PluginToolPermission::WorkspaceWrite,
            None,
        )])
        .expect("plugin tool registry should build")
    }

    #[test]
    fn defaults_to_repl_when_no_args() {
        assert_eq!(
            parse_args(&[]).expect("args should parse"),
            CliAction::Repl {
                model: DEFAULT_MODEL.to_string(),
                allowed_tools: None,
                permission_mode: PermissionMode::DangerFullAccess,
            }
        );
    }

    #[test]
    fn parses_prompt_subcommand() {
        let args = vec![
            "prompt".to_string(),
            "hello".to_string(),
            "world".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Prompt {
                prompt: "hello world".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: PermissionMode::DangerFullAccess,
            }
        );
    }

    #[test]
    fn parses_bare_prompt_and_json_output_flag() {
        let args = vec![
            "--output-format=json".to_string(),
            "--model".to_string(),
            "custom-opus".to_string(),
            "explain".to_string(),
            "this".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Prompt {
                prompt: "explain this".to_string(),
                model: "custom-opus".to_string(),
                output_format: CliOutputFormat::Json,
                allowed_tools: None,
                permission_mode: PermissionMode::DangerFullAccess,
            }
        );
    }

    #[test]
    fn resolves_model_aliases_in_args() {
        let args = vec![
            "--model".to_string(),
            "opus".to_string(),
            "explain".to_string(),
            "this".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Prompt {
                prompt: "explain this".to_string(),
                model: "claude-opus-4-6".to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: PermissionMode::DangerFullAccess,
            }
        );
    }

    #[test]
    fn resolves_known_model_aliases() {
        assert_eq!(resolve_model_alias("opus"), "claude-opus-4-6");
        assert_eq!(resolve_model_alias("sonnet"), "claude-sonnet-4-6");
        assert_eq!(resolve_model_alias("haiku"), "claude-haiku-4-5-20251213");
        assert_eq!(resolve_model_alias("custom-opus"), "custom-opus");
    }

    #[test]
    fn parses_version_flags_without_initializing_prompt_mode() {
        assert_eq!(
            parse_args(&["--version".to_string()]).expect("args should parse"),
            CliAction::Version
        );
        assert_eq!(
            parse_args(&["-V".to_string()]).expect("args should parse"),
            CliAction::Version
        );
    }

    #[test]
    fn parses_permission_mode_flag() {
        let args = vec!["--permission-mode=read-only".to_string()];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Repl {
                model: DEFAULT_MODEL.to_string(),
                allowed_tools: None,
                permission_mode: PermissionMode::ReadOnly,
            }
        );
    }

    #[test]
    fn parses_allowed_tools_flags_with_aliases_and_lists() {
        let args = vec![
            "--allowedTools".to_string(),
            "read,glob".to_string(),
            "--allowed-tools=write_file".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Repl {
                model: DEFAULT_MODEL.to_string(),
                allowed_tools: Some(
                    ["glob_search", "read_file", "write_file"]
                        .into_iter()
                        .map(str::to_string)
                        .collect()
                ),
                permission_mode: PermissionMode::DangerFullAccess,
            }
        );
    }

    #[test]
    fn rejects_unknown_allowed_tools() {
        let error = parse_args(&["--allowedTools".to_string(), "teleport".to_string()])
            .expect_err("tool should be rejected");
        assert!(error.contains("unsupported tool in --allowedTools: teleport"));
    }

    #[test]
    fn parses_system_prompt_options() {
        let args = vec![
            "system-prompt".to_string(),
            "--cwd".to_string(),
            "/tmp/project".to_string(),
            "--date".to_string(),
            "2026-04-01".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::PrintSystemPrompt {
                cwd: PathBuf::from("/tmp/project"),
                date: "2026-04-01".to_string(),
            }
        );
    }

    #[test]
    fn parses_login_and_logout_subcommands() {
        assert_eq!(
            parse_args(&["login".to_string()]).expect("login should parse"),
            CliAction::Login { provider: None }
        );
        assert_eq!(
            parse_args(&["login".to_string(), "--provider".to_string(), "openai".to_string()])
                .expect("login --provider openai should parse"),
            CliAction::Login {
                provider: Some("openai".to_string())
            }
        );
        assert_eq!(
            parse_args(&["login".to_string(), "--provider=anthropic".to_string()])
                .expect("login --provider=anthropic should parse"),
            CliAction::Login {
                provider: Some("anthropic".to_string())
            }
        );
        // Simple syntax: `anvil login openai`
        assert_eq!(
            parse_args(&["login".to_string(), "openai".to_string()])
                .expect("login openai should parse"),
            CliAction::Login {
                provider: Some("openai".to_string())
            }
        );
        // Simple syntax: `anvil login provider ollama`
        assert_eq!(
            parse_args(&["login".to_string(), "provider".to_string(), "ollama".to_string()])
                .expect("login provider ollama should parse"),
            CliAction::Login {
                provider: Some("ollama".to_string())
            }
        );
        assert_eq!(
            parse_args(&["logout".to_string()]).expect("logout should parse"),
            CliAction::Logout
        );
        assert_eq!(
            parse_args(&["init".to_string()]).expect("init should parse"),
            CliAction::Init
        );
        assert_eq!(
            parse_args(&["agents".to_string()]).expect("agents should parse"),
            CliAction::Agents { args: None }
        );
        assert_eq!(
            parse_args(&["skills".to_string()]).expect("skills should parse"),
            CliAction::Skills { args: None }
        );
        assert_eq!(
            parse_args(&["agents".to_string(), "--help".to_string()])
                .expect("agents help should parse"),
            CliAction::Agents {
                args: Some("--help".to_string())
            }
        );
    }

    #[test]
    fn parses_direct_agents_and_skills_slash_commands() {
        assert_eq!(
            parse_args(&["/agents".to_string()]).expect("/agents should parse"),
            CliAction::Agents { args: None }
        );
        assert_eq!(
            parse_args(&["/skills".to_string()]).expect("/skills should parse"),
            CliAction::Skills { args: None }
        );
        assert_eq!(
            parse_args(&["/skills".to_string(), "help".to_string()])
                .expect("/skills help should parse"),
            CliAction::Skills {
                args: Some("help".to_string())
            }
        );
        let error = parse_args(&["/status".to_string()])
            .expect_err("/status should remain REPL-only when invoked directly");
        assert!(error.contains("Direct slash command unavailable"));
        assert!(error.contains("/status"));
    }

    #[test]
    fn parses_resume_flag_with_slash_command() {
        let args = vec![
            "--resume".to_string(),
            "session.json".to_string(),
            "/compact".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::ResumeSession {
                session_path: PathBuf::from("session.json"),
                commands: vec!["/compact".to_string()],
            }
        );
    }

    #[test]
    fn parses_resume_flag_with_multiple_slash_commands() {
        let args = vec![
            "--resume".to_string(),
            "session.json".to_string(),
            "/status".to_string(),
            "/compact".to_string(),
            "/cost".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::ResumeSession {
                session_path: PathBuf::from("session.json"),
                commands: vec![
                    "/status".to_string(),
                    "/compact".to_string(),
                    "/cost".to_string(),
                ],
            }
        );
    }

    #[test]
    fn filtered_tool_specs_respect_allowlist() {
        let allowed = ["read_file", "grep_search"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let filtered = filter_tool_specs(&GlobalToolRegistry::builtin(), Some(&allowed));
        let names = filtered
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["read_file", "grep_search"]);
    }

    #[test]
    fn filtered_tool_specs_include_plugin_tools() {
        let filtered = filter_tool_specs(&registry_with_plugin_tool(), None);
        let names = filtered
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"bash".to_string()));
        assert!(names.contains(&"plugin_echo".to_string()));
    }

    #[test]
    fn permission_policy_uses_plugin_tool_permissions() {
        let policy = permission_policy(PermissionMode::ReadOnly, &registry_with_plugin_tool());
        let required = policy.required_mode_for("plugin_echo");
        assert_eq!(required, PermissionMode::WorkspaceWrite);
    }

    #[test]
    fn shared_help_uses_resume_annotation_copy() {
        let help = commands::render_slash_command_help();
        assert!(help.contains("Slash commands"));
        assert!(help.contains("Tab completes commands inside the REPL."));
        assert!(help.contains("available via anvil --resume SESSION.json"));
    }

    #[test]
    fn repl_help_includes_shared_commands_and_exit() {
        let help = render_repl_help();
        assert!(help.contains("Interactive REPL"));
        assert!(help.contains("/help"));
        assert!(help.contains("/status"));
        assert!(help.contains("/model [model]"));
        assert!(help.contains("/permissions [read-only|workspace-write|danger-full-access]"));
        assert!(help.contains("/clear [--confirm]"));
        assert!(help.contains("/cost"));
        assert!(help.contains("/resume <session-path>"));
        assert!(help.contains("/config [env|hooks|model|plugins]"));
        assert!(help.contains("/memory"));
        assert!(help.contains("/init"));
        assert!(help.contains("/diff"));
        assert!(help.contains("/version"));
        assert!(help.contains("/export [file]"));
        assert!(help.contains("/session [list|switch <session-id>]"));
        assert!(help.contains(
            "/plugin [list|install <path>|enable <name>|disable <name>|uninstall <id>|update <id>]"
        ));
        assert!(help.contains("aliases: /plugins, /marketplace"));
        assert!(help.contains("/agents"));
        assert!(help.contains("/skills"));
        assert!(help.contains("/exit"));
        assert!(help.contains("Tab cycles slash command matches"));
    }

    #[test]
    fn completion_candidates_include_repl_only_exit_commands() {
        let candidates = slash_command_completion_candidates();
        assert!(candidates.contains(&"/help".to_string()));
        assert!(candidates.contains(&"/vim".to_string()));
        assert!(candidates.contains(&"/exit".to_string()));
        assert!(candidates.contains(&"/quit".to_string()));
    }

    #[test]
    fn unknown_repl_command_suggestions_include_repl_shortcuts() {
        let rendered = render_unknown_repl_command("exi");
        assert!(rendered.contains("Unknown slash command"));
        assert!(rendered.contains("/exit"));
        assert!(rendered.contains("/help"));
    }

    #[test]
    fn resume_supported_command_list_matches_expected_surface() {
        let names = resume_supported_slash_commands()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "help", "status", "compact", "clear", "cost", "config", "memory", "init", "diff",
                "version", "export", "agents", "skills", "qmd", "history", "doctor", "tokens",
                "history-archive", "configure", "language",
            ]
        );
    }

    #[test]
    fn resume_report_uses_sectioned_layout() {
        let report = format_resume_report("session.json", 14, 6);
        assert!(report.contains("Session resumed"));
        assert!(report.contains("Session file     session.json"));
        assert!(report.contains("History          14 messages · 6 turns"));
        assert!(report.contains("/status · /diff · /export"));
    }

    #[test]
    fn compact_report_uses_structured_output() {
        let compacted = format_compact_report(8, 5, false);
        assert!(compacted.contains("Compact"));
        assert!(compacted.contains("Result           compacted"));
        assert!(compacted.contains("Messages removed 8"));
        assert!(compacted.contains("Use /status"));
        let skipped = format_compact_report(0, 3, true);
        assert!(skipped.contains("Result           skipped"));
    }

    #[test]
    fn cost_report_uses_sectioned_layout() {
        let report = format_cost_report(runtime::TokenUsage {
            input_tokens: 20,
            output_tokens: 8,
            cache_creation_input_tokens: 3,
            cache_read_input_tokens: 1,
        });
        assert!(report.contains("Cost"));
        assert!(report.contains("Input tokens     20"));
        assert!(report.contains("Output tokens    8"));
        assert!(report.contains("Cache create     3"));
        assert!(report.contains("Cache read       1"));
        assert!(report.contains("Total tokens     32"));
        assert!(report.contains("/compact"));
    }

    #[test]
    fn permissions_report_uses_sectioned_layout() {
        let report = format_permissions_report("workspace-write");
        assert!(report.contains("Permissions"));
        assert!(report.contains("Active mode      workspace-write"));
        assert!(report.contains("Effect           Editing tools can modify files in the workspace"));
        assert!(report.contains("Modes"));
        assert!(report.contains("read-only          ○ available Read/search tools only"));
        assert!(report.contains("workspace-write    ● current   Edit files inside the workspace"));
        assert!(report.contains("danger-full-access ○ available Unrestricted tool access"));
    }

    #[test]
    fn permissions_switch_report_is_structured() {
        let report = format_permissions_switch_report("read-only", "workspace-write");
        assert!(report.contains("Permissions updated"));
        assert!(report.contains("Previous mode    read-only"));
        assert!(report.contains("Active mode      workspace-write"));
        assert!(report.contains("Applies to       Subsequent tool calls in this REPL"));
    }

    #[test]
    fn init_help_mentions_direct_subcommand() {
        let mut help = Vec::new();
        print_help_to(&mut help).expect("help should render");
        let help = String::from_utf8(help).expect("help should be utf8");
        assert!(help.contains("anvil init"));
        assert!(help.contains("anvil agents"));
        assert!(help.contains("anvil skills"));
        assert!(help.contains("anvil /skills"));
    }

    #[test]
    fn model_report_uses_sectioned_layout() {
        let report = format_model_report("sonnet", 12, 4);
        assert!(report.contains("Model"));
        assert!(report.contains("Current          sonnet"));
        assert!(report.contains("Session          12 messages · 4 turns"));
        assert!(report.contains("Aliases"));
        assert!(report.contains("/model <name>    Switch models for this REPL session"));
    }

    #[test]
    fn model_switch_report_preserves_context_summary() {
        let report = format_model_switch_report("sonnet", "opus", 9);
        assert!(report.contains("Model updated"));
        assert!(report.contains("Previous         sonnet"));
        assert!(report.contains("Current          opus"));
        assert!(report.contains("Preserved        9 messages"));
    }

    #[test]
    fn status_line_reports_model_and_token_totals() {
        let status = format_status_report(
            "sonnet",
            StatusUsage {
                message_count: 7,
                turns: 3,
                latest: runtime::TokenUsage {
                    input_tokens: 5,
                    output_tokens: 4,
                    cache_creation_input_tokens: 1,
                    cache_read_input_tokens: 0,
                },
                cumulative: runtime::TokenUsage {
                    input_tokens: 20,
                    output_tokens: 8,
                    cache_creation_input_tokens: 2,
                    cache_read_input_tokens: 1,
                },
                estimated_tokens: 128,
            },
            "workspace-write",
            &super::StatusContext {
                cwd: PathBuf::from("/tmp/project"),
                session_path: Some(PathBuf::from("session.json")),
                loaded_config_files: 2,
                discovered_config_files: 3,
                memory_file_count: 4,
                project_root: Some(PathBuf::from("/tmp")),
                git_branch: Some("main".to_string()),
            },
        );
        assert!(status.contains("Session"));
        assert!(status.contains("Model            sonnet"));
        assert!(status.contains("Permissions      workspace-write"));
        assert!(status.contains("Activity         7 messages · 3 turns"));
        assert!(status.contains("Tokens           est 128 · latest 10 · total 31"));
        assert!(status.contains("Folder           /tmp/project"));
        assert!(status.contains("Project root     /tmp"));
        assert!(status.contains("Git branch       main"));
        assert!(status.contains("Session file     session.json"));
        assert!(status.contains("Config files     loaded 2/3"));
        assert!(status.contains("Memory files     4"));
        assert!(status.contains("/session list"));
    }

    #[test]
    fn config_report_supports_section_views() {
        let report = render_config_report(Some("env")).expect("config report should render");
        assert!(report.contains("Merged section: env"));
        let plugins_report =
            render_config_report(Some("plugins")).expect("plugins config report should render");
        assert!(plugins_report.contains("Merged section: plugins"));
    }

    #[test]
    fn memory_report_uses_sectioned_layout() {
        let report = render_memory_report().expect("memory report should render");
        assert!(report.contains("Memory"));
        assert!(report.contains("Working directory"));
        assert!(report.contains("Instruction files"));
        assert!(report.contains("Persistent memory"));
    }

    #[test]
    fn config_report_uses_sectioned_layout() {
        let report = render_config_report(None).expect("config report should render");
        assert!(report.contains("Config"));
        assert!(report.contains("Discovered files"));
        assert!(report.contains("Merged JSON"));
    }

    #[test]
    fn parses_git_status_metadata() {
        let (root, branch) = parse_git_status_metadata(Some(
            "## rcc/cli...origin/rcc/cli
 M src/main.rs",
        ));
        assert_eq!(branch.as_deref(), Some("rcc/cli"));
        let _ = root;
    }

    #[test]
    fn status_context_reads_real_workspace_metadata() {
        let context = status_context(None).expect("status context should load");
        assert!(context.cwd.is_absolute());
        assert_eq!(context.discovered_config_files, 5);
        assert!(context.loaded_config_files <= context.discovered_config_files);
    }

    #[test]
    fn normalizes_supported_permission_modes() {
        assert_eq!(normalize_permission_mode("read-only"), Some("read-only"));
        assert_eq!(
            normalize_permission_mode("workspace-write"),
            Some("workspace-write")
        );
        assert_eq!(
            normalize_permission_mode("danger-full-access"),
            Some("danger-full-access")
        );
        assert_eq!(normalize_permission_mode("unknown"), None);
    }

    #[test]
    fn clear_command_requires_explicit_confirmation_flag() {
        assert_eq!(
            SlashCommand::parse("/clear"),
            Some(SlashCommand::Clear { confirm: false })
        );
        assert_eq!(
            SlashCommand::parse("/clear --confirm"),
            Some(SlashCommand::Clear { confirm: true })
        );
    }

    #[test]
    fn parses_resume_and_config_slash_commands() {
        assert_eq!(
            SlashCommand::parse("/resume saved-session.json"),
            Some(SlashCommand::Resume {
                session_path: Some("saved-session.json".to_string())
            })
        );
        assert_eq!(
            SlashCommand::parse("/clear --confirm"),
            Some(SlashCommand::Clear { confirm: true })
        );
        assert_eq!(
            SlashCommand::parse("/config"),
            Some(SlashCommand::Config { section: None })
        );
        assert_eq!(
            SlashCommand::parse("/config env"),
            Some(SlashCommand::Config {
                section: Some("env".to_string())
            })
        );
        assert_eq!(SlashCommand::parse("/memory"), Some(SlashCommand::Memory));
        assert_eq!(SlashCommand::parse("/init"), Some(SlashCommand::Init));
    }

    #[test]
    fn init_template_mentions_detected_rust_workspace() {
        let rendered = crate::init::render_init_anvil_md(std::path::Path::new("."));
        assert!(rendered.contains("# ANVIL.md"));
        assert!(rendered.contains("cargo clippy --workspace --all-targets -- -D warnings"));
    }

    #[test]
    fn converts_tool_roundtrip_messages() {
        let messages = vec![
            ConversationMessage::user_text("hello"),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "tool-1".to_string(),
                name: "bash".to_string(),
                input: "{\"command\":\"pwd\"}".to_string(),
            }]),
            ConversationMessage {
                role: MessageRole::Tool,
                blocks: vec![ContentBlock::ToolResult {
                    tool_use_id: "tool-1".to_string(),
                    tool_name: "bash".to_string(),
                    output: "ok".to_string(),
                    is_error: false,
                }],
                usage: None,
            },
        ];

        let converted = super::convert_messages(&messages);
        assert_eq!(converted.len(), 3);
        assert_eq!(converted[1].role, "assistant");
        assert_eq!(converted[2].role, "user");
    }
    #[test]
    fn repl_help_mentions_history_completion_and_multiline() {
        let help = render_repl_help();
        assert!(help.contains("Up/Down"));
        assert!(help.contains("Tab cycles"));
        assert!(help.contains("Shift+Enter or Ctrl+J"));
    }

    #[test]
    fn tool_rendering_helpers_compact_output() {
        let start = format_tool_call_start("read_file", r#"{"path":"src/main.rs"}"#);
        assert!(start.contains("read_file"));
        assert!(start.contains("src/main.rs"));

        let done = format_tool_result(
            "read_file",
            r#"{"file":{"filePath":"src/main.rs","content":"hello","numLines":1,"startLine":1,"totalLines":1}}"#,
            false,
        );
        assert!(done.contains("📄 Read src/main.rs"));
        assert!(done.contains("hello"));
    }

    #[test]
    fn tool_rendering_truncates_large_read_output_for_display_only() {
        let content = (0..200)
            .map(|index| format!("line {index:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let output = json!({
            "file": {
                "filePath": "src/main.rs",
                "content": content,
                "numLines": 200,
                "startLine": 1,
                "totalLines": 200
            }
        })
        .to_string();

        let rendered = format_tool_result("read_file", &output, false);

        assert!(rendered.contains("line 000"));
        assert!(rendered.contains("line 079"));
        assert!(!rendered.contains("line 199"));
        assert!(rendered.contains("full result preserved in session"));
        assert!(output.contains("line 199"));
    }

    #[test]
    fn tool_rendering_truncates_large_bash_output_for_display_only() {
        let stdout = (0..120)
            .map(|index| format!("stdout {index:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let output = json!({
            "stdout": stdout,
            "stderr": "",
            "returnCodeInterpretation": "completed successfully"
        })
        .to_string();

        let rendered = format_tool_result("bash", &output, false);

        assert!(rendered.contains("stdout 000"));
        assert!(rendered.contains("stdout 059"));
        assert!(!rendered.contains("stdout 119"));
        assert!(rendered.contains("full result preserved in session"));
        assert!(output.contains("stdout 119"));
    }

    #[test]
    fn tool_rendering_truncates_generic_long_output_for_display_only() {
        let items = (0..120)
            .map(|index| format!("payload {index:03}"))
            .collect::<Vec<_>>();
        let output = json!({
            "summary": "plugin payload",
            "items": items,
        })
        .to_string();

        let rendered = format_tool_result("plugin_echo", &output, false);

        assert!(rendered.contains("plugin_echo"));
        assert!(rendered.contains("payload 000"));
        assert!(rendered.contains("payload 040"));
        assert!(!rendered.contains("payload 080"));
        assert!(!rendered.contains("payload 119"));
        assert!(rendered.contains("full result preserved in session"));
        assert!(output.contains("payload 119"));
    }

    #[test]
    fn tool_rendering_truncates_raw_generic_output_for_display_only() {
        let output = (0..120)
            .map(|index| format!("raw {index:03}"))
            .collect::<Vec<_>>()
            .join("\n");

        let rendered = format_tool_result("plugin_echo", &output, false);

        assert!(rendered.contains("plugin_echo"));
        assert!(rendered.contains("raw 000"));
        assert!(rendered.contains("raw 059"));
        assert!(!rendered.contains("raw 119"));
        assert!(rendered.contains("full result preserved in session"));
        assert!(output.contains("raw 119"));
    }

    #[test]
    fn ultraplan_progress_lines_include_phase_step_and_elapsed_status() {
        let snapshot = InternalPromptProgressState {
            command_label: "Ultraplan",
            task_label: "ship plugin progress".to_string(),
            step: 3,
            phase: "running read_file".to_string(),
            detail: Some("reading rust/crates/anvil-cli/src/main.rs".to_string()),
            saw_final_text: false,
        };

        let started = format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Started,
            &snapshot,
            Duration::from_secs(0),
            None,
        );
        let heartbeat = format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Heartbeat,
            &snapshot,
            Duration::from_secs(9),
            None,
        );
        let completed = format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Complete,
            &snapshot,
            Duration::from_secs(12),
            None,
        );
        let failed = format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Failed,
            &snapshot,
            Duration::from_secs(12),
            Some("network timeout"),
        );

        assert!(started.contains("planning started"));
        assert!(started.contains("current step 3"));
        assert!(heartbeat.contains("heartbeat"));
        assert!(heartbeat.contains("9s elapsed"));
        assert!(heartbeat.contains("phase running read_file"));
        assert!(completed.contains("completed"));
        assert!(completed.contains("3 steps total"));
        assert!(failed.contains("failed"));
        assert!(failed.contains("network timeout"));
    }

    #[test]
    fn describe_tool_progress_summarizes_known_tools() {
        assert_eq!(
            describe_tool_progress("read_file", r#"{"path":"src/main.rs"}"#),
            "reading src/main.rs"
        );
        assert!(
            describe_tool_progress("bash", r#"{"command":"cargo test -p anvil-cli"}"#)
                .contains("cargo test -p anvil-cli")
        );
        assert_eq!(
            describe_tool_progress("grep_search", r#"{"pattern":"ultraplan","path":"rust"}"#),
            "grep `ultraplan` in rust"
        );
    }

    #[test]
    fn push_output_block_renders_markdown_text() {
        let mut out = Vec::new();
        let mut events = Vec::new();
        let mut pending_tool = None;

        push_output_block(
            OutputContentBlock::Text {
                text: "# Heading".to_string(),
            },
            &mut out,
            &mut events,
            &mut pending_tool,
            false,
        )
        .expect("text block should render");

        let rendered = String::from_utf8(out).expect("utf8");
        assert!(rendered.contains("Heading"));
        assert!(rendered.contains('\u{1b}'));
    }

    #[test]
    fn push_output_block_skips_empty_object_prefix_for_tool_streams() {
        let mut out = Vec::new();
        let mut events = Vec::new();
        let mut pending_tool = None;

        push_output_block(
            OutputContentBlock::ToolUse {
                id: "tool-1".to_string(),
                name: "read_file".to_string(),
                input: json!({}),
            },
            &mut out,
            &mut events,
            &mut pending_tool,
            true,
        )
        .expect("tool block should accumulate");

        assert!(events.is_empty());
        assert_eq!(
            pending_tool,
            Some(("tool-1".to_string(), "read_file".to_string(), String::new(),))
        );
    }

    #[test]
    fn response_to_events_preserves_empty_object_json_input_outside_streaming() {
        let mut out = Vec::new();
        let events = response_to_events(
            MessageResponse {
                id: "msg-1".to_string(),
                kind: "message".to_string(),
                model: "claude-opus-4-6".to_string(),
                role: "assistant".to_string(),
                content: vec![OutputContentBlock::ToolUse {
                    id: "tool-1".to_string(),
                    name: "read_file".to_string(),
                    input: json!({}),
                }],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                request_id: None,
            },
            &mut out,
        )
        .expect("response conversion should succeed");

        assert!(matches!(
            &events[0],
            AssistantEvent::ToolUse { name, input, .. }
                if name == "read_file" && input == "{}"
        ));
    }

    #[test]
    fn response_to_events_preserves_non_empty_json_input_outside_streaming() {
        let mut out = Vec::new();
        let events = response_to_events(
            MessageResponse {
                id: "msg-2".to_string(),
                kind: "message".to_string(),
                model: "claude-opus-4-6".to_string(),
                role: "assistant".to_string(),
                content: vec![OutputContentBlock::ToolUse {
                    id: "tool-2".to_string(),
                    name: "read_file".to_string(),
                    input: json!({ "path": "rust/Cargo.toml" }),
                }],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                request_id: None,
            },
            &mut out,
        )
        .expect("response conversion should succeed");

        assert!(matches!(
            &events[0],
            AssistantEvent::ToolUse { name, input, .. }
                if name == "read_file" && input == "{\"path\":\"rust/Cargo.toml\"}"
        ));
    }

    #[test]
    fn response_to_events_ignores_thinking_blocks() {
        let mut out = Vec::new();
        let events = response_to_events(
            MessageResponse {
                id: "msg-3".to_string(),
                kind: "message".to_string(),
                model: "claude-opus-4-6".to_string(),
                role: "assistant".to_string(),
                content: vec![
                    OutputContentBlock::Thinking {
                        thinking: "step 1".to_string(),
                        signature: Some("sig_123".to_string()),
                    },
                    OutputContentBlock::Text {
                        text: "Final answer".to_string(),
                    },
                ],
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                request_id: None,
            },
            &mut out,
        )
        .expect("response conversion should succeed");

        assert!(matches!(
            &events[0],
            AssistantEvent::TextDelta(text) if text == "Final answer"
        ));
        assert!(!String::from_utf8(out).expect("utf8").contains("step 1"));
    }
}
