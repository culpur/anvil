mod agents;
mod auth;
mod cmd_ai;
mod cmd_static;
mod configure;
mod file_drop;
mod format_tool;
mod help;
mod init;
mod input;
mod providers;
mod render;
mod screensaver;
mod session;
mod tui;
mod update;
mod utils;
mod vault;
mod wizard;

// Re-export utilities so that existing call sites throughout this file
// (handle_repl_command, run_command_for_tui, etc.) continue to resolve without changes.
pub(crate) use utils::*;

rust_i18n::i18n!("../../locales", fallback = "en");


use std::collections::BTreeSet;
use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use api::{
    detect_provider_kind, max_tokens_for_model, provider_display_name, ProviderKind, ToolDefinition,
};

use commands::{
    handle_agents_slash_command, handle_plugins_slash_command, handle_skills_slash_command,
    render_command_detailed_help, SlashCommand,
};
use compat_harness::{extract_manifest, UpstreamPaths};
use render::{
    render_welcome_banner, BannerInfo, StatusLine,
    ThinkingIndicator,
};
use runtime::{
    format_package_detail, format_package_list, load_system_prompt, pricing_for_model,
    render_history_context, render_qmd_context,
    ArchiveEntry, BlockingHubClient, CompactionConfig, CompletedTaskInfo,
    ConfigLoader, ContentBlock, ConversationRuntime, CronDaemon,
    HistoryArchiver, MessageRole,
    PermissionMode, QmdClient, Session, TaskManager, TokenUsage, UsageTracker,
};
use crossterm::terminal;
use serde_json::json;
use tools::GlobalToolRegistry;
use tui::{AnvilTui, ReadResult, TuiEvent, TuiSender};

use auth::{
    query_anthropic_models, run_anthropic_login, run_login, run_logout, run_ollama_setup,
    run_openai_apikey_setup,
};
use session::{
    create_managed_session_handle, format_relative_timestamp, list_managed_sessions,
    render_session_list, resolve_session_reference, sessions_dir,
    SessionHandle,
};
use help::print_help;
use update::{check_for_update, run_self_update};
use vault::{run_vault_command_impl, write_curl_auth_header};
use wizard::{anvil_config_json_exists, run_first_run_wizard};
use providers::{
    build_plugin_manager, build_runtime, build_runtime_with_tui_slot, resolve_cli_auth_source,
    CliPermissionPrompter, DefaultRuntimeClient, CliToolExecutor,
    InternalPromptProgressReporter,
    final_assistant_text, collect_tool_uses, collect_tool_results,
    slash_command_completion_candidates,
};

/// A shared slot for the TUI sender.  Created once at startup and cloned into
/// `DefaultRuntimeClient` and `CliToolExecutor`.  When the TUI is active the
/// inner value is `Some`; setting it to `None` restores plain-stdout mode.
pub(crate) type TuiSenderSlot = Arc<Mutex<Option<TuiSender>>>;

const DEFAULT_MODEL: &str = "claude-opus-4-6";
pub(crate) const DEFAULT_DATE: &str = env!("BUILD_DATE");
const DEFAULT_OAUTH_CALLBACK_PORT: u16 = 4545;
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const BUILD_TARGET: &str = env!("TARGET");
pub(crate) const GIT_SHA: &str = env!("GIT_SHA");
const INTERNAL_PROGRESS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);

pub(crate) type AllowedToolSet = BTreeSet<String>;

fn main() {
    // Install panic hook to clean up terminal state (disable mouse capture, leave alt screen)
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen
        );
        default_hook(info);
    }));

    if let Err(error) = run() {
        // Ensure terminal is cleaned up on error exit too
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen
        );
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
        CliAction::FirstRunWizard => run_first_run_wizard(),
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
    /// Run the interactive first-run setup wizard.
    FirstRunWizard,
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
    // Read default model and Ollama config from config.json
    let mut model = {
        let config_path = anvil_home_dir().join("config.json");
        if let Ok(data) = std::fs::read_to_string(&config_path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&data) {
                // Set OLLAMA_HOST env var from config so agent threads inherit it
                if let Some(ollama_url) = val.pointer("/providers/ollama/url").and_then(|v| v.as_str()) {
                    if !ollama_url.is_empty() && std::env::var("OLLAMA_HOST").is_err() {
                        std::env::set_var("OLLAMA_HOST", ollama_url);
                    }
                }
                val.get("default_model")
                    .and_then(|m| m.as_str()).map_or_else(|| DEFAULT_MODEL.to_string(), String::from)
            } else {
                DEFAULT_MODEL.to_string()
            }
        } else {
            DEFAULT_MODEL.to_string()
        }
    };
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
            "--first-run" | "--setup" => {
                run_first_run_wizard();
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
        "setup" | "first-run" => Ok(CliAction::FirstRunWizard),
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
            Ok(CliAction::Repl { model, allowed_tools, permission_mode })
        }
        "login" => {
            // Support: `anvil login`, `anvil login anthropic`, `anvil login provider openai`,
            //          `anvil login --provider openai`, `anvil login --provider=openai`
            let mut provider: Option<String> = None;
            let mut idx = 1;
            while idx < rest.len() {
                match rest[idx].as_str() {
                    // `anvil login provider <name>` / `anvil login --provider <name>` (backward compat)
                    "provider" | "--provider" => {
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
        Some(SlashCommand::Help { .. }) => Ok(CliAction::Help),
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

pub(crate) fn filter_tool_specs(
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
pub(crate) struct StatusContext {
    cwd: PathBuf,
    session_path: Option<PathBuf>,
    loaded_config_files: usize,
    discovered_config_files: usize,
    memory_file_count: usize,
    project_root: Option<PathBuf>,
    git_branch: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StatusUsage {
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

pub(crate) fn parse_git_status_metadata(status: Option<&str>) -> (Option<PathBuf>, Option<String>) {
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
        SlashCommand::Help { ref command } => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(if let Some(ref cmd) = command {
                render_command_detailed_help(cmd)
                    .unwrap_or_else(render_repl_help)
            } else {
                render_repl_help()
            }),
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
        | SlashCommand::Sleep
        | SlashCommand::Think
        | SlashCommand::Fast
        | SlashCommand::ReviewPr { .. }
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

// ─── Vault startup helpers ─────────────────────────────────────────────────────

/// Initialise the vault for the current session.
///
/// Covers three scenarios on startup:
///
/// 1. **First use, credentials.json exists, vault not initialized**: prompt the
///    user to migrate their existing plaintext credentials into the vault.
/// 2. **Vault initialized, not yet unlocked this session**: prompt for the
///    master password once and unlock into the session cache.
/// 3. **No vault, no credentials.json**: nothing to do — the wizard handles it.
///
/// After unlocking the vault, any credentials stored there are injected into
/// process environment variables so the rest of the code finds them via the
/// standard `std::env::var` paths without further changes.
fn startup_vault_init() {
    if !io::stdout().is_terminal() {
        // Non-interactive mode — skip interactive prompts.
        load_credentials_to_env();
        return;
    }

    let home_dir = anvil_home_dir();
    let creds_json = home_dir.join("credentials.json");
    let vault_initialized = runtime::vault_is_initialized();

    // ── Migration path ────────────────────────────────────────────────────────
    if creds_json.exists() && !vault_initialized {
        println!();
        println!("\x1b[1;33mAnvil Security Notice\x1b[0m");
        println!("\x1b[33m{}\x1b[0m", "\u{2500}".repeat(41));
        println!("  Your API keys are stored in plaintext at:");
        println!("    {}", creds_json.display());
        println!();
        println!("  Set up the encrypted vault now to protect them.");
        println!("  [1] Migrate to encrypted vault (recommended)");
        println!("  [s] Skip \u{2014} keep using plaintext credentials.json");
        println!();
        print!("  Choice [1]: ");
        let _ = io::stdout().flush();
        let mut choice = String::new();
        let _ = io::stdin().read_line(&mut choice);
        if !matches!(choice.trim().to_ascii_lowercase().as_str(), "s" | "skip") {
            let pw = loop {
                let p1 = match rpassword::prompt_password("  Set master password: ") {
                    Ok(p) => p,
                    Err(_) => {
                        print!("  Set master password: ");
                        let _ = io::stdout().flush();
                        let mut b = String::new();
                        let _ = io::stdin().read_line(&mut b);
                        b.trim().to_string()
                    }
                };
                if p1.is_empty() {
                    println!("  Password must not be empty.");
                    continue;
                }
                let p2 = match rpassword::prompt_password("  Confirm master password: ") {
                    Ok(p) => p,
                    Err(_) => {
                        print!("  Confirm: ");
                        let _ = io::stdout().flush();
                        let mut b = String::new();
                        let _ = io::stdin().read_line(&mut b);
                        b.trim().to_string()
                    }
                };
                if p1 != p2 {
                    println!("  Passwords do not match. Try again.");
                    continue;
                }
                break p1;
            };
            let mut vm = runtime::VaultManager::with_default_dir();
            match vm.setup(&pw) {
                Ok(()) => {
                    drop(vm);
                    match runtime::init_session_vault(&pw) {
                        Ok(true) => {
                            let migrated = migrate_credentials_to_vault(&creds_json);
                            println!("\x1b[32m  Vault created and unlocked.\x1b[0m");
                            if migrated > 0 {
                                println!("  Migrated {migrated} credential(s) into vault.");
                                let bak = creds_json.with_extension("json.bak");
                                if fs::rename(&creds_json, &bak).is_ok() {
                                    println!(
                                        "  Renamed credentials.json \u{2192} credentials.json.bak"
                                    );
                                }
                            }
                        }
                        _ => {
                            println!(
                                "\x1b[33m  Vault created but session unlock failed.\x1b[0m"
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!("  Vault setup error: {e}");
                }
            }
        }
        println!();
        load_credentials_to_env();
        return;
    }

    // ── Unlock existing vault ─────────────────────────────────────────────────
    if vault_initialized && !runtime::vault_is_session_unlocked() {
        eprint!("  Vault master password: ");
        match rpassword::read_password() {
            Ok(pw) if !pw.is_empty() => {
                match runtime::init_session_vault(&pw) {
                    Ok(true) => {}
                    Ok(false) => eprintln!("  Vault not initialized."),
                    Err(e) => eprintln!("  Vault unlock failed: {e}"),
                }
            }
            _ => {}
        }
    }

    load_credentials_to_env();
}

/// Migrate plaintext credentials.json entries into the session vault.
/// Skips the `"oauth"` key which is managed by the OAuth subsystem.
/// Returns the count of migrated entries.
fn migrate_credentials_to_vault(creds_path: &std::path::Path) -> usize {
    let Ok(data) = fs::read_to_string(creds_path) else {
        return 0;
    };
    let Ok(root) =
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data)
    else {
        return 0;
    };
    let mut count = 0usize;
    for (key, val) in &root {
        if key == "oauth" {
            continue;
        }
        if let Some(secret) = val.as_str() {
            if runtime::vault_session_upsert(key, secret).is_ok() {
                count += 1;
            }
        }
    }
    count
}

/// Inject credentials from the vault (or plaintext credentials.json fallback)
/// into process environment variables so the rest of the codebase can find
/// them via `std::env::var`.  Variables already present in the environment are
/// never overwritten, preserving explicit user shell overrides.
fn load_credentials_to_env() {
    const KEY_ENV_PAIRS: &[(&str, &str)] = &[
        ("anthropic_api_key", "ANTHROPIC_API_KEY"),
        ("openai_api_key", "OPENAI_API_KEY"),
        ("xai_api_key", "XAI_API_KEY"),
        ("ollama_host", "OLLAMA_HOST"),
        ("ollama_api_key", "OLLAMA_API_KEY"),
    ];
    for &(cred_label, env_var) in KEY_ENV_PAIRS {
        if std::env::var(env_var).map(|v| !v.is_empty()).unwrap_or(false) {
            continue;
        }
        // Vault first.
        if let Some(val) = runtime::vault_session_get(cred_label) {
            if !val.is_empty() {
                std::env::set_var(env_var, &val);
                continue;
            }
        }
        // Plaintext fallback.
        if let Ok(creds_path) = runtime::credentials_path() {
            if let Ok(data) = fs::read_to_string(&creds_path) {
                if let Ok(root) =
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data)
                {
                    if let Some(val) = root.get(cred_label).and_then(|v| v.as_str()) {
                        if !val.is_empty() {
                            std::env::set_var(env_var, val);
                        }
                    }
                }
            }
        }
    }
}

fn run_repl(
    model: String,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
) -> Result<(), Box<dyn std::error::Error>> {
    // Auto-detect first run: if ~/.anvil/config.json does not exist yet,
    // guide the user through the setup wizard before entering the REPL.
    let model = if io::stdout().is_terminal() && !anvil_config_json_exists() {
        run_first_run_wizard();
        // Re-read the config to pick up the user's chosen default model
        let config_path = anvil_home_dir().join("config.json");
        if let Ok(data) = fs::read_to_string(&config_path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&data) {
                val.get("default_model")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&model)
                    .to_string()
            } else {
                model
            }
        } else {
            model
        }
    } else {
        model
    };

    // Unlock the vault (or offer migration) once before the REPL starts.
    // After run_first_run_wizard the vault is already unlocked in the session
    // cache, so startup_vault_init is a no-op in that case.
    startup_vault_init();

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
    // Cache Ollama models once at startup (non-blocking for tab completions)
    tui::init_ollama_model_cache();

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
    let _cron_daemon = if std::env::var("ANVIL_NO_CRON").as_deref() == Ok("1") {
        None
    } else {
        Some(CronDaemon::start())
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
    // ── Screensaver state ──────────────────────────────────────────────────────
    let mut screensaver_state: Option<screensaver::FurnaceScreensaver> = None;
    let mut last_input_time = Instant::now();

    'outer: loop {
        // Check for background task completions.
        task_check_instant = inject_task_notifications_tui(&mut cli, &mut tui, task_check_instant);

        // ── Agent manager: poll for completed agents ───────────────────────
        // Drain completed agents and inject their results as system messages.
        {
            let completed_agents = cli
                .agent_manager
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .poll();
            for (id, result) in completed_agents {
                let status = if result.success { "completed" } else { "failed" };
                let summary: String = result.output.lines().take(3).collect::<Vec<_>>().join(" | ");
                tui.push_system(format!(
                    "Agent #{id} {status} in {:.1}s{}",
                    result.duration.as_secs_f64(),
                    if summary.is_empty() { String::new() } else { format!(": {summary}") },
                ));
            }
        }

        // Refresh the TUI agent panel rows from the current manager state.
        {
            let rows: Vec<(usize, String, String, String, &'static str)> = cli
                .agent_manager
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .agents()
                .iter()
                .map(|a| {
                    (
                        a.id,
                        a.agent_type.label().to_string(),
                        a.task.clone(),
                        a.elapsed_str(),
                        a.status.icon(),
                    )
                })
                .collect();
            tui.update_agent_rows(rows);
        }

        // Check if the background update check completed.
        if let Ok(mut slot) = update_check.try_lock() {
            if let Some(msg) = slot.take() {
                tui.set_update_available(msg);
            }
        }

        // ── Screensaver: auto-activate on 15-min idle ──────────────────────
        if screensaver_state.is_none()
            && last_input_time.elapsed() >= screensaver::IDLE_TIMEOUT
        {
            let lines = tui.capture_screen_text();
            screensaver_state = Some(screensaver::FurnaceScreensaver::new(lines));
        }

        // ── Screensaver: run animation tick and handle input ───────────────
        if let Some(ref mut ss) = screensaver_state {
            let result = tui.read_input_screensaver(ss)?;
            let still_active = ss.is_active();
            if !still_active {
                screensaver_state = None;
                last_input_time = Instant::now();
            }
            match result {
                tui::ReadResult::Exit => {
                    cli.persist_session()?;
                    break 'outer;
                }
                _ => continue,
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
                last_input_time = Instant::now();
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if matches!(trimmed, "/exit" | "/quit") {
                    cli.persist_session()?;
                    break 'outer;
                }

                // /sleep — activate the furnace screensaver immediately.
                if matches!(trimmed, "/sleep" | "/screensaver" | "/furnace") {
                    let lines = tui.capture_screen_text();
                    screensaver_state = Some(screensaver::FurnaceScreensaver::new(lines));
                    continue;
                }

                // /tab is TUI-only — handle before SlashCommand dispatch.
                if let Some(tab_rest) = trimmed.strip_prefix("/tab") {
                    let rest = tab_rest.trim();
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
                                let _ = writeln!(msg, "  [{id}] {name}{marker}{active}");
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

    let _cron_daemon = if std::env::var("ANVIL_NO_CRON").as_deref() == Ok("1") {
        None
    } else {
        Some(CronDaemon::start())
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
    /// Whether vim keybindings are requested; propagated to the `LineEditor`.
    vim_mode: bool,
    thinking_enabled: bool,
    /// Fast mode: lower `max_tokens` and prepend a concise-response instruction.
    fast_mode: bool,
    /// Sub-agent manager — tracks spawned agents and their status.
    /// Wrapped in Arc<Mutex<>> so it can be shared with `CliToolExecutor`.
    agent_manager: Arc<Mutex<agents::AgentManager>>,
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
        // Shared agent manager — owned here for TUI polling, shared with
        // CliToolExecutor so tool calls can register real agent threads.
        let agent_manager: Arc<Mutex<agents::AgentManager>> =
            Arc::new(Mutex::new(agents::AgentManager::new()));
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
            agent_manager.clone(),
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
            agent_manager,
            thinking_enabled: {
                let cfg = anvil_home_dir().join("config.json");
                std::fs::read_to_string(&cfg).ok()
                    .and_then(|d| serde_json::from_str::<serde_json::Value>(&d).ok())
                    .and_then(|v| v.get("thinking_enabled").and_then(serde_json::Value::as_bool))
                    .unwrap_or(false)
            },
            fast_mode: false,
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
            SlashCommand::Model { ref model } if model.is_some() => {
                let new_model = model.as_deref().unwrap();
                let previous = self.model.clone();
                self.model = new_model.to_string();
                let msg_count = self.runtime.session().messages.len();
                tui.set_model(self.model.clone());
                tui.push_system(format_model_switch_report(&previous, &self.model, msg_count));
                return Ok(false);
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
                let mut buf = String::new();
                let _ = io::stdin().read_line(&mut buf);
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
                        let _ = write!(msg, "\n  {}. {} ({:.0}%)\n     {}\n",
                            i + 1,
                            r.file,
                            r.score * 100.0,
                            snippet_short
                        );
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
    /// Returns (`output_text`, `session_changed`).
    fn run_command_for_tui(
        &mut self,
        command: SlashCommand,
    ) -> Result<(String, bool), Box<dyn std::error::Error>> {
        Ok(match command {
            SlashCommand::Help { ref command } => {
                let text = if let Some(ref cmd) = command {
                    render_command_detailed_help(cmd).unwrap_or_else(render_repl_help)
                } else {
                    render_repl_help()
                };
                (text, false)
            }
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
                (format!("Anvil CLI v{VERSION}\nBuild: {BUILD_TARGET} / {GIT_SHA}"), false)
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
                    .output().map_or_else(|_| "Not in a git repository.".to_string(), |o| String::from_utf8_lossy(&o.stdout).to_string());
                (if diff.trim().is_empty() { "No uncommitted changes.".to_string() } else { diff }, false)
            }
            SlashCommand::Compact => {
                self.compact()?;
                ("Session compacted.".to_string(), false)
            }
            SlashCommand::Agents { ref args } => {
                // Route subcommands that target the live agent manager
                // (list, view, stop, clear) to the manager first.  If the
                // subcommand is not recognised locally, fall through to the
                // global handle_agents_slash_command which handles hub lookups.
                let arg_str = args.as_deref().unwrap_or("").trim();
                let is_manager_cmd = arg_str.is_empty()
                    || arg_str.starts_with("list")
                    || arg_str.starts_with("view")
                    || arg_str.starts_with("stop")
                    || arg_str.starts_with("clear");
                if is_manager_cmd {
                    let msg = self
                        .agent_manager
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .handle_command(Some(arg_str));
                    (msg, false)
                } else {
                    let cwd = env::current_dir().unwrap_or_default();
                    let output = handle_agents_slash_command(args.as_deref(), &cwd);
                    (output.unwrap_or_else(|e| format!("Error: {e}")), false)
                }
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
            SlashCommand::Think => {
                self.thinking_enabled = !self.thinking_enabled;
                (format!(
                    "Thinking mode: {}",
                    if self.thinking_enabled { "ON — model will show reasoning" } else { "OFF — standard responses" }
                ), false)
            }
            SlashCommand::Fast => {
                match self.toggle_fast_mode() {
                    Ok(msg) => (msg, false),
                    Err(e) => (format!("fast mode error: {e}"), false),
                }
            }
            SlashCommand::ReviewPr { number } => {
                let msg = self.run_review_pr_command(number.as_deref());
                (msg, false)
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
    #[allow(clippy::unused_self)]
    fn run_undo(&self) -> Result<String, Box<dyn std::error::Error>> {
        cmd_static::run_undo()
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
    #[allow(clippy::unused_self)]
    fn run_pin(&self, path: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
        cmd_static::run_pin(path)
    }

    /// `/unpin <path>` — remove a pinned file.
    #[allow(clippy::unused_self)]
    fn run_unpin(&self, path: &str) -> Result<String, Box<dyn std::error::Error>> {
        cmd_static::run_unpin(path)
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
            self.agent_manager.clone(),
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

    /// `/fast` — toggle fast mode.
    ///
    /// When fast mode is active the system prompt is prepended with a
    /// "Be concise and direct." instruction and the runtime is rebuilt with the
    /// modified system prompt.  Toggling again restores the original prompt.
    fn toggle_fast_mode(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        const FAST_PREFIX: &str = "Be concise and direct.";
        self.fast_mode = !self.fast_mode;

        // Rebuild system_prompt: prepend or remove the fast-mode prefix.
        if self.fast_mode {
            // Only prepend if not already present (idempotent).
            if !self.system_prompt.first().map_or("", std::string::String::as_str).contains(FAST_PREFIX) {
                self.system_prompt.insert(0, FAST_PREFIX.to_string());
            }
        } else {
            self.system_prompt.retain(|s| s.as_str() != FAST_PREFIX);
        }

        // Rebuild the runtime so the new system prompt takes effect.
        let session = self.runtime.session().clone();
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
            self.agent_manager.clone(),
        )?;

        let msg = if self.fast_mode {
            "Fast mode ON — responses will be concise and max_tokens is reduced.".to_string()
        } else {
            "Fast mode OFF — responses restored to normal length.".to_string()
        };
        Ok(msg)
    }

    // `/review-pr [<number>]` — fetch a GitHub PR diff and run an AI review.

    /// `/web <query>` — run a web search and display results inline.
    #[allow(clippy::unused_self)]
    fn run_web_search_command(&self, query: &str) -> String {
        cmd_static::run_web_search_command(query)
    }

    /// `/generate-image <prompt>` — generate an image via `OpenAI` and download it locally.
    ///
    /// Supports an optional `--wp <post-id>` flag to upload the result to `WordPress` as
    /// the featured image for the given post.
    fn run_generate_image(&self, prompt: &str, wp_post_id: Option<&str>) -> String {
        if prompt.trim().is_empty() {
            return "Usage: /image <prompt>\n       /image --wp <post-id> <prompt>".to_string();
        }

        // Resolve the OpenAI API key using the priority chain:
        //   1. OPENAI_API_KEY environment variable (explicit user override)
        //   2. Encrypted vault (if vault is unlocked for this session)
        //   3. Plaintext credentials.json (backward-compat fallback)
        let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        let api_key = if api_key.is_empty() {
            // Vault
            runtime::vault_session_get("openai_api_key")
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    // Plaintext fallback
                    runtime::credentials_path()
                        .ok()
                        .and_then(|p| fs::read_to_string(&p).ok())
                        .and_then(|data| {
                            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
                                &data,
                            )
                            .ok()
                            .and_then(|root| {
                                root.get("openai_api_key")
                                    .and_then(|v| v.as_str())
                                    .map(ToOwned::to_owned)
                            })
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

        // Call the API.  Write the auth header to a temp file (mode 0o600) so the
        // token is never visible in the process argument list.
        let auth_header_path = match write_curl_auth_header(&api_key) {
            Ok(p) => p,
            Err(e) => return format!("Failed to prepare auth header: {e}"),
        };
        let output = std::process::Command::new("curl")
            .args([
                "-s", "-X", "POST",
                "https://api.openai.com/v1/images/generations",
                "-H", &format!("@{}", auth_header_path.display()),
                "-H", "Content-Type: application/json",
                "-d", &body.to_string(),
            ])
            .output();
        let _ = fs::remove_file(&auth_header_path);

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
        let downloads = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), |h| PathBuf::from(h).join("Downloads"));
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
            let _ = write!(result, "\nUploading to WordPress post {post_id}…");
            let _ = io::stdout().flush();
            let upload_result = self.upload_wp_featured_image(&path_str, post_id, &api_key);
            result.push('\n');
            result.push_str(&upload_result);
        }

        result
    }

    /// Upload a local image file to `WordPress` as the featured image for a post.
    ///
    /// Requires `WP_URL`, `WP_USER`, and `WP_APP_PASSWORD` environment variables,
    /// which are the standard variables used by the existing `generate_article_image.sh` script.
    #[allow(clippy::unused_self)]
    fn upload_wp_featured_image(&self, path: &str, post_id: &str, openai_key: &str) -> String {
        cmd_static::upload_wp_featured_image(path, post_id, openai_key)
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
        let memory_ok = home.as_ref().is_some_and(|h| h.join(".anvil").is_dir());
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
        let ctx_window: usize = 200_000;
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
                                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                                    if let Some(models) = val.get("models").and_then(|m| m.as_array()) {
                                        for m in models {
                                            let name = m.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                                            let size = m.get("size").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                                            let gb = size / 1_000_000_000.0;
                                            let _ = writeln!(out, "  {name:<30} {gb:.1}GB");
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

    #[allow(clippy::unused_self)]
    fn format_search_tool_result(&self, query: &str, input: &serde_json::Value) -> String {
        cmd_static::format_search_tool_result(query, input)
    }

    /// `/failover` — AI provider failover chain management.
    #[allow(clippy::unused_self)]
    fn run_failover_command(&self, action: Option<&str>) -> String {
        cmd_static::run_failover_command(action)
    }

    fn handle_repl_command(
        &mut self,
        command: SlashCommand,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        Ok(match command {
            SlashCommand::Help { ref command } => {
                let text = if let Some(ref cmd) = command {
                    render_command_detailed_help(cmd).unwrap_or_else(render_repl_help)
                } else {
                    render_repl_help()
                };
                println!("{text}");
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
            SlashCommand::Sleep => {
                println!("Sleep mode is not yet supported in this build.");
                false
            }
            SlashCommand::Think => {
                self.thinking_enabled = !self.thinking_enabled;
                println!(
                    "Thinking mode: {}",
                    if self.thinking_enabled { "ON — model will show reasoning" } else { "OFF — standard responses" }
                );
                false
            }
            SlashCommand::Fast => {
                match self.toggle_fast_mode() {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => eprintln!("fast mode error: {e}"),
                }
                false
            }
            SlashCommand::ReviewPr { number } => {
                println!("{}", self.run_review_pr_command(number.as_deref()));
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
            self.agent_manager.clone(),
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
            self.agent_manager.clone(),
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
            self.agent_manager.clone(),
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
            self.agent_manager.clone(),
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
            self.agent_manager.clone(),
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
                    self.agent_manager.clone(),
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
            self.agent_manager.clone(),
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
            self.agent_manager.clone(),
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
            self.agent_manager.clone(),
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
                String::new,
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

    // ─── Feature 3: Semantic Code Search ─────────────────────────────────────

    #[allow(clippy::unused_self)]
    fn run_semantic_search(&self, args: Option<&str>) -> String {
        cmd_static::run_semantic_search(args)
    }

    // ─── Feature 4: Docker / Container Awareness ──────────────────────────────

    fn run_docker_command(args: Option<&str>) -> String {
        cmd_static::run_docker_command(args)
    }

    // ─── Feature 5: Test Generation ───────────────────────────────────────────

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

        if let Some(cherry_rest) = args.strip_prefix("cherry-pick") {
            let sha = cherry_rest.trim();
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

    // ─── Features 8-12 ───────────────────────────────────────────────────────

    // -----------------------------------------------------------------------
    // Feature 8 — Screenshot / clipboard image input
    // -----------------------------------------------------------------------

    /// `/screenshot` — capture screen via OS tool, inject as vision content block.
    #[allow(clippy::unused_self)]
    fn run_screenshot_command(&self) -> String {
        cmd_static::run_screenshot_command()
    }

    // -----------------------------------------------------------------------
    // Feature 9 — Database tools
    // -----------------------------------------------------------------------

    // `/db [connect <url>|schema|query <sql>|migrate]`

    // -----------------------------------------------------------------------
    // Feature 10 — Security scanning
    // -----------------------------------------------------------------------

    /// `/security [scan|secrets|deps|report]`
    #[allow(clippy::unused_self)]
    fn run_security_command(&self, args: Option<&str>) -> String {
        cmd_static::run_security_command(args)
    }

    #[allow(dead_code, clippy::unused_self)]
    fn run_security_scan(&self) -> String {
        cmd_static::run_security_scan()
    }

    #[allow(dead_code, clippy::unused_self)]
    fn run_security_secrets(&self) -> String {
        cmd_static::run_security_secrets()
    }

    #[allow(dead_code, clippy::unused_self)]
    fn run_security_deps(&self) -> String {
        cmd_static::run_security_deps()
    }

    // -----------------------------------------------------------------------
    // Feature 11 — API development helpers
    // -----------------------------------------------------------------------

    // `/api [spec <file>|mock <spec>|test <url>|docs]`

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

    fn run_voice_command(args: Option<&str>) -> String {
        cmd_static::run_voice_command(args)
    }

    fn run_collab_command(args: Option<&str>) -> String {
        cmd_static::run_collab_command(args)
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
                        Err(e) => { let _ = write!(out, "\n{label}\n  (error: {e})\n"); }
                    }
                }
                out.push_str("\nRun /hub <category> for more, or /hub install <name> to install.");
                out
            }
        }
    }

    #[allow(clippy::unused_self)]
    fn run_language_command(&self, lang: Option<&str>) -> String {
        cmd_static::run_language_command(lang)
    }

    // ─── Feature 1: LSP Autocomplete ─────────────────────────────────────────

    // ─── Feature 2: Jupyter Notebook Support ─────────────────────────────────

    // ─── Feature 3: Kubernetes Management ────────────────────────────────────

    fn run_k8s_command(args: Option<&str>) -> String {
        cmd_static::run_k8s_command(args)
    }

    // ─── Feature 4: Terraform/IaC ────────────────────────────────────────────

    fn run_iac_command(args: Option<&str>) -> String {
        cmd_static::run_iac_command(args)
    }

    // ─── Feature 5: CI/CD Pipeline Builder ───────────────────────────────────

    // ─── Feature 6: Code Review ───────────────────────────────────────────────

    // ─── Feature 7: Dependency Graph ─────────────────────────────────────────

    fn run_deps_command(args: Option<&str>) -> String {
        cmd_static::run_deps_command(args)
    }

    // ─── Feature 8: Monorepo Awareness ───────────────────────────────────────

    fn run_mono_command(args: Option<&str>) -> String {
        cmd_static::run_mono_command(args)
    }

    // ─── Feature 9: Browser Automation ───────────────────────────────────────

    fn run_browser_command(args: Option<&str>) -> String {
        cmd_static::run_browser_command(args)
    }

    // ─── Feature 10: Notifications ───────────────────────────────────────────

    fn run_notify_command(args: Option<&str>) -> String {
        cmd_static::run_notify_command(args)
    }

    // ─── Feature 21 — Credential Vault ───────────────────────────────────────
    #[allow(clippy::unused_self)]
    fn run_vault_command(&mut self, args: Option<&str>) -> String {
        run_vault_command_impl(args)
    }

    // Feature stubs for pre-existing commands K-P that were dispatched but never implemented.

    // ─── Feature 11 — Codebase migration ─────────────────────────────────────

    // ─── Feature 12 — Regex builder / tester ─────────────────────────────────

    // ─── Feature 13 — SSH session manager ────────────────────────────────────

    fn run_ssh_command(args: Option<&str>) -> String {
        cmd_static::run_ssh_command(args)
    }

    // ─── Feature 14 — Log analysis ───────────────────────────────────────────

    // ─── Feature 15 — Markdown preview ───────────────────────────────────────

    fn run_markdown_command(args: Option<&str>) -> String {
        cmd_static::run_markdown_command(args)
    }

    // ─── Feature 16 — Snippet library ────────────────────────────────────────

    fn run_snippets_command(args: Option<&str>) -> String {
        cmd_static::run_snippets_command(args)
    }

    // ─── Feature 17 — AI fine-tuning assistant ────────────────────────────────

    // ─── Feature 18 — Webhook manager ────────────────────────────────────────

    fn run_webhook_command(args: Option<&str>) -> String {
        cmd_static::run_webhook_command(args)
    }

    // ─── Feature 20 — Plugin SDK ──────────────────────────────────────────────

    fn run_plugin_sdk_command(args: Option<&str>) -> String {
        cmd_static::run_plugin_sdk_command(args)
    }

    /// Read a string value from `~/.anvil/config.json` with a fallback default.
    #[allow(clippy::unused_self)]
    fn anvil_config_str(&self, key: &str, default: &str) -> String {
        cmd_static::anvil_config_str(key, default)
    }



    #[allow(clippy::unused_self)]
    fn run_teleport(&self, target: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        cmd_static::run_teleport(target)
    }

    fn run_debug_tool_call(&self) -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", render_last_tool_debug_report(self.runtime.session())?);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
