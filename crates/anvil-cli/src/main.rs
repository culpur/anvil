// Edition 2024: env::set_var/remove_var require unsafe
#![allow(unsafe_code)]

mod agents;
mod auth;
mod cmd_ai;
mod cmd_static;
mod commands_extra;
mod commands_util;
mod configure;
mod file_drop;
mod format_tool;
mod help;
mod init;
mod input;
mod mcp_server_mode;
mod mcp_server_tools;
mod providers;
mod remote_control;
mod render;
mod respawn;
mod share;
mod screensaver;
mod session;
mod session_meta;
mod tui;
mod check;
mod project;
mod setup;
mod uninstall;
mod skill_eval;
mod update;
mod upgrade;
mod utils;
mod vault;
mod wizard;

// Re-export utilities so that existing call sites throughout this file
// (handle_repl_command, run_command_for_tui, etc.) continue to resolve without changes.
pub(crate) use utils::{command_exists, detect_project_type_for_pipeline, extract_notebook_cell, git_output, git_status_ok, lsp_binary_for_lang, parse_line_range, parse_titled_body, recent_user_context, run_test_suite, sanitize_generated_message, shell_output_or_err, shell_quote, truncate_for_prompt, write_temp_text_file, anvil_home_dir, anvil_pinned_path, dirs_next_home, json_escape, load_pinned_paths, regex_escape, render_teleport_report, run_language_command_static, save_pinned_paths, send_desktop_notification, format_number, parse_token_count, run_init, append_slash_command_suggestions, normalize_permission_mode, render_version_report, render_repl_help, format_status_report, status_context, render_config_report, render_memory_report, init_anvil_md, render_diff_report, resolve_export_path, render_export_text, render_export_markdown, render_configure_static, build_system_prompt, build_system_prompt_with_identity, friendly_provider_label, run_theme_command, render_mode_unavailable, render_unknown_repl_command, run_git_stash_list, run_git_stash_op, render_last_tool_debug_report, save_output_style, load_output_style};

pub(crate) use configure::config_data_to_json;

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
    format_suggestions_hint, handle_agents_slash_command,
    handle_plugins_slash_command, handle_skills_slash_command, load_skill_body, match_triggers,
    render_command_detailed_help, AgentSubcommand, SkillSubcommand, SlashCommand,
    bundled_catalogue, compose_agent, format_traits_listing, ComposeError,
};
use compat_harness::{extract_manifest, UpstreamPaths};
use render::{
    render_welcome_banner, BannerInfo, StatusLine,
    ThinkingIndicator,
};
use runtime::{
    check_plugin_install_policy, format_package_detail, format_package_list, load_requirements,
    load_system_prompt, pricing_for_model, render_history_context, render_qmd_context,
    ArchiveEntry, BlockingHubClient, CompactionConfig, CompletedTaskInfo,
    ConfigLoader, ConversationRuntime, CronDaemon,
    EffortLevel, HistoryArchiver, NotificationKind, NotificationPayload, OutputStyle,
    PermissionMode, PolicyCheckError, QmdClient, Session, TaskManager, TokenUsage, UsageTracker,
};
use crossterm::terminal;
use serde_json::json;
use tools::GlobalToolRegistry;
use tui::{AnvilTui, ConfigureAction, ReadResult, TuiEvent, TuiSender};

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
use check::run_check;
use setup::run_setup_wizard;
use uninstall::run_uninstall;
use update::{check_for_update, run_self_update};
use upgrade::run_upgrade;
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

// Thread-local storage for the RespawnContext captured at startup.
// Using a std::cell::OnceCell ensures the context is written exactly once
// (in main) and can be read from any handler.
thread_local! {
    static RESPAWN_CTX: std::cell::OnceCell<respawn::RespawnContext> =
        const { std::cell::OnceCell::new() };
}

/// Obtain a clone of the stored [`respawn::RespawnContext`], or a safe
/// default (with all unsafe flags set) if called before `main()` initialises
/// the cell (only possible in tests).
fn get_respawn_ctx() -> respawn::RespawnContext {
    RESPAWN_CTX.with(|cell| {
        cell.get().cloned().unwrap_or_else(|| respawn::RespawnContext {
            argv0: String::new(),
            args: vec![],
            env_captured: vec![],
            launched_from_pipe: true,   // mark as unsafe so tests don't exec
            launched_from_cargo: true,
            launched_from_debugger: false,
            no_respawn_flag: true,
        })
    })
}

fn main() {
    // Capture the launch context as early as possible, before any argument
    // parsing, so that argv[0] and the raw args are intact.
    //
    // SAFETY: RespawnContext::capture reads env vars and args — no unsafe
    // code here; the #[allow(unsafe_code)] lives in respawn.rs.
    let respawn_ctx = respawn::RespawnContext::capture();

    // Soft PID lock: warn (but don't block) if another Anvil process is
    // already running.
    if let Some(other_pid) = respawn::read_running_pid() {
        eprintln!(
            "Warning: another Anvil process (PID {other_pid}) appears to be running."
        );
    }

    // Write our PID file; it is removed automatically when _pid_guard drops.
    let _pid_guard = respawn::PidFileGuard::new();

    // Check for a resume marker left by a previous respawn.
    if let Some(state) = respawn::load_resume_state() {
        eprintln!(
            "Resuming session {} (respawned {}s ago).",
            state.session_id,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                .saturating_sub(state.written_at)
        );
        respawn::clear_resume_state();
    }

    // Store the respawn context in a thread-local so that the /restart handler
    // can access it without needing to thread it through every call site.
    RESPAWN_CTX.with(|cell| {
        let _ = cell.set(respawn_ctx);
    });

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

/// Render a `/profile` command response for contexts that don't yet have a
/// live runtime with access to the loaded profile map (resume path, plain REPL,
/// TUI fallback).  The TUI path will eventually call into the richer live
/// implementation; this provides a consistent baseline message.
fn render_profile_command(action: Option<&str>) -> String {
    // Read active profile from env (set by --profile flag or ANVIL_PROFILE).
    let active = std::env::var("ANVIL_PROFILE")
        .ok()
        .filter(|s| !s.is_empty());

    match action {
        None | Some("list") => {
            match active {
                Some(ref name) => format!(
                    "Active profile: {name}\n\
                     Use /profile show to inspect its fields, or --profile <name> to change it."
                ),
                None => "No active profile. Use /profile create <name> to add one, \
                         or --profile <name> at startup to activate one."
                    .to_string(),
            }
        }
        Some(rest) if rest.starts_with("use ") => {
            let name = rest[4..].trim();
            if name.is_empty() {
                "Usage: /profile use <name>".to_string()
            } else {
                // Session-scoped switch: update the env var so resolve_active_profile_owned picks it up.
                // SAFETY: see existing env::set_var usage in parse_args.
                unsafe { std::env::set_var("ANVIL_PROFILE", name); }
                format!("Active profile switched to \"{name}\" for this session.")
            }
        }
        Some(rest) if rest.starts_with("show") => {
            let arg = rest["show".len()..].trim();
            let target = if arg.is_empty() {
                active.as_deref().map(ToOwned::to_owned)
            } else {
                Some(arg.to_string())
            };
            match target {
                None => "No profile specified and none active. Usage: /profile show <name>".to_string(),
                Some(name) => format!(
                    "Profile: {name}\n\
                     (Full field listing requires a TUI session with the config loaded.)"
                ),
            }
        }
        Some(rest) if rest.starts_with("create ") => {
            let name = rest[7..].trim();
            if name.is_empty() {
                "Usage: /profile create <name>".to_string()
            } else {
                format!(
                    "To create profile \"{name}\", add it to the `profiles` section in \
                     ~/.anvil/settings.json and restart Anvil."
                )
            }
        }
        Some(rest) if rest.starts_with("delete ") => {
            let name = rest[7..].trim();
            if name.is_empty() {
                "Usage: /profile delete <name>".to_string()
            } else {
                format!(
                    "To delete profile \"{name}\", remove it from the `profiles` section in \
                     ~/.anvil/settings.json and restart Anvil."
                )
            }
        }
        Some(other) => format!(
            "Unknown /profile sub-command: {other}\n\
             Usage: /profile [list|use <name>|show [<name>]|create <name>|delete <name>]"
        ),
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
        CliAction::Setup => run_setup_wizard(),
        CliAction::Check => {
            let fails = run_check();
            std::process::exit(if fails == 0 { 0 } else { 1 });
        }
        CliAction::Upgrade => run_upgrade(),
        CliAction::Uninstall => run_uninstall(),
        CliAction::Repl {
            model,
            allowed_tools,
            permission_mode,
        } => run_repl(model, allowed_tools, permission_mode)?,
        CliAction::Help => print_help(),
        CliAction::Continue => run_continue()?,
        CliAction::Sessions => print_sessions_standalone()?,
        CliAction::SkillEval { args } => {
            skill_eval::run_skill_eval(args)?;
        }
        CliAction::Project { opts } => {
            let anvil_home = ::runtime::default_config_home();
            let mut stdout = std::io::stdout().lock();
            project::run_purge(&anvil_home, &opts, &mut stdout)
                .map_err(|e| format!("project purge failed: {e}"))?;
        }
        CliAction::EmitSchema => {
            let schema = ::runtime::emit_config_schema();
            let json = serde_json::to_string_pretty(&schema)
                .map_err(|e| format!("schema serialisation failed: {e}"))?;
            println!("{json}");
        }
        CliAction::McpServer { read_only, tools_filter } => {
            let config = mcp_server_mode::McpServerConfig { read_only, tools_filter };
            let stdin = std::io::stdin().lock();
            let stdout = std::io::stdout().lock();
            mcp_server_mode::run_mcp_server(
                std::io::BufReader::new(stdin),
                stdout,
                &config,
            )?;
        }
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
    /// Run the interactive first-run setup wizard (existing REPL wizard).
    FirstRunWizard,
    /// Run the post-install setup wizard (new, called by installer scripts).
    Setup,
    /// Print the installation health checklist.
    Check,
    /// Upgrade Anvil to the latest release.
    Upgrade,
    /// Uninstall Anvil binary and optionally ~/.anvil/.
    Uninstall,
    /// Run a three-arm skill evaluation.
    SkillEval {
        args: skill_eval::SkillEvalArgs,
    },
    /// Per-workspace state management (CC parity FEAT-39).
    Project {
        opts: project::PurgeOptions,
    },
    /// Print the JSON Schema for ~/.anvil/config.json to stdout, then exit.
    EmitSchema,
    /// Expose Anvil as an MCP server on stdio.
    McpServer {
        /// Refuse write/edit/bash tools when true.
        read_only: bool,
        /// When Some, restrict published tools to exactly this list of names.
        tools_filter: Option<Vec<String>>,
    },
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
                if let Some(ollama_url) = val.pointer("/providers/ollama/url").and_then(|v| v.as_str())
                    && !ollama_url.is_empty() && std::env::var("OLLAMA_HOST").is_err() {
                        unsafe { std::env::set_var("OLLAMA_HOST", ollama_url); }
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
    // W4: --profile <name> sets the active profile for this session.
    // ANVIL_PROFILE env var is also checked (lower precedence).
    let mut cli_profile: Option<String> = std::env::var("ANVIL_PROFILE")
        .ok()
        .filter(|s| !s.is_empty());
    // FEAT-42: --plugin-url accumulates URLs; --plugin-sha256 attaches to the
    // *next* --plugin-url and may also appear as `--plugin-url URL --plugin-sha256 HEX`
    // in either order.  We collect raw flags here and resolve them once the
    // arg loop is done so order doesn't matter.
    let mut plugin_dirs: Vec<String> = Vec::new();
    let mut plugin_urls: Vec<(String, Option<String>)> = Vec::new();
    let mut pending_sha256: Option<String> = None;

    while index < args.len() {
        match args[index].as_str() {
            "--version" | "-V" => {
                wants_version = true;
                index += 1;
            }
            "--emit-schema" => {
                return Ok(CliAction::EmitSchema);
            }
            "--update" => {
                run_self_update();
                std::process::exit(0);
            }
            "--check" => {
                return Ok(CliAction::Check);
            }
            "--uninstall" => {
                return Ok(CliAction::Uninstall);
            }
            "--setup" => {
                return Ok(CliAction::Setup);
            }
            "--first-run" => {
                return Ok(CliAction::FirstRunWizard);
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
            // W4: --profile <name>  (overrides ANVIL_PROFILE env var)
            "--profile" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --profile".to_string())?;
                cli_profile = Some(value.clone());
                index += 2;
            }
            flag if flag.starts_with("--profile=") => {
                cli_profile = Some(flag[10..].to_string());
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
            // FEAT-42: --plugin-dir <path>.  Path may be a directory (existing
            // behaviour) or a `.zip` archive (extracted to a session temp dir).
            "--plugin-dir" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --plugin-dir".to_string())?;
                plugin_dirs.push(value.clone());
                index += 2;
            }
            flag if flag.starts_with("--plugin-dir=") => {
                plugin_dirs.push(flag[13..].to_string());
                index += 1;
            }
            // FEAT-42: --plugin-url <https://...> fetches a plugin .zip for
            // the current session.  Optional --plugin-sha256 HEX verifies it.
            "--plugin-url" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --plugin-url".to_string())?;
                plugin_urls.push((value.clone(), pending_sha256.take()));
                index += 2;
            }
            flag if flag.starts_with("--plugin-url=") => {
                plugin_urls.push((flag[13..].to_string(), pending_sha256.take()));
                index += 1;
            }
            "--plugin-sha256" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --plugin-sha256".to_string())?;
                // Attach to the most recent --plugin-url if one is already
                // registered without a hash; otherwise, defer to the next.
                if let Some(last) = plugin_urls.last_mut()
                    && last.1.is_none()
                {
                    last.1 = Some(value.clone());
                } else {
                    pending_sha256 = Some(value.clone());
                }
                index += 2;
            }
            flag if flag.starts_with("--plugin-sha256=") => {
                let value = flag[16..].to_string();
                if let Some(last) = plugin_urls.last_mut()
                    && last.1.is_none()
                {
                    last.1 = Some(value);
                } else {
                    pending_sha256 = Some(value);
                }
                index += 1;
            }
            other => {
                rest.push(other.to_string());
                index += 1;
            }
        }
    }

    // FEAT-42: resolve session plugin sources.  Errors here are surfaced as
    // CLI errors so the operator sees `https-only`, `zip too large`, etc.
    // up front rather than as a silent no-load.
    if !plugin_dirs.is_empty() || !plugin_urls.is_empty() {
        // W10: enforce requirements.toml plugin install policy before proceeding.
        // Load the policy from the standard candidate paths; if no policy file is
        // present this returns a default (permissive) policy with an empty source
        // path, and check_plugin_install_policy is a no-op in that case.
        let (policy, policy_source) = load_requirements();
        // --plugin-dir installs count as a plain install (no URL).
        if !plugin_dirs.is_empty() {
            if let Err(msg) = check_plugin_install_policy(&policy, &policy_source, false) {
                return Err(msg);
            }
        }
        // --plugin-url installs carry has_url = true.
        if !plugin_urls.is_empty() {
            if let Err(msg) = check_plugin_install_policy(&policy, &policy_source, true) {
                return Err(msg);
            }
        }
        plugins::sweep_stale_session_dirs();
    }
    for raw in plugin_dirs {
        let path = PathBuf::from(&raw);
        let prepared = plugins::prepare_plugin_dir_source(&path)
            .map_err(|error| format!("--plugin-dir {raw}: {error}"))?;
        plugins::register_session_source(prepared);
    }
    for (url, sha256) in plugin_urls {
        let prepared = plugins::prepare_plugin_url_source(&url, sha256.as_deref())
            .map_err(|error| format!("--plugin-url {url}: {error}"))?;
        plugins::register_session_source(prepared);
    }
    if let Some(stranded) = pending_sha256 {
        return Err(format!(
            "--plugin-sha256 {stranded} has no matching --plugin-url"
        ));
    }

    // W4: If --profile was specified, it wins over ANVIL_PROFILE env var.
    // We normalise both into ANVIL_PROFILE so that the rest of the runtime
    // (config loader + /profile commands) can read a single authoritative source.
    if let Some(ref profile_name) = cli_profile {
        // SAFETY: standard env manipulation; only unsafe in edition 2024.
        unsafe { std::env::set_var("ANVIL_PROFILE", profile_name); }
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
        "emit-schema" => Ok(CliAction::EmitSchema),
        "sessions" | "session-list" => Ok(CliAction::Sessions),
        "first-run" => Ok(CliAction::FirstRunWizard),
        "check" => Ok(CliAction::Check),
        "upgrade" => Ok(CliAction::Upgrade),
        "uninstall" => Ok(CliAction::Uninstall),
        "setup" => Ok(CliAction::Setup),
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
        "skill-eval" => {
            let sub_args = rest[1..].to_vec();
            if sub_args.iter().any(|a| a == "--help" || a == "-h") {
                print!("{}", skill_eval::USAGE);
                std::process::exit(0);
            }
            let parsed = skill_eval::parse_skill_eval_args(&sub_args)?;
            Ok(CliAction::SkillEval { args: parsed })
        }
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
        "project" => {
            // `anvil project purge [path] [flags]` (CC parity FEAT-39).
            // We currently support only the `purge` action; future actions
            // (e.g. `project list`, `project info`) would dispatch here too.
            match rest.get(1).map(String::as_str) {
                Some("purge") => {
                    let opts = project::parse_purge_args(&rest[2..])?;
                    Ok(CliAction::Project { opts })
                }
                Some(other) => Err(format!(
                    "unknown `anvil project` action: {other} (expected: purge)"
                )),
                None => Err("missing action for `anvil project` (try: purge)".to_string()),
            }
        }
        "mcp-server" => parse_mcp_server_args(&rest[1..]),
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

/// Parse arguments for `anvil mcp-server [--read-only] [--tools <list>]`.
fn parse_mcp_server_args(args: &[String]) -> Result<CliAction, String> {
    let mut read_only = false;
    let mut tools_filter: Option<Vec<String>> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--read-only" => {
                read_only = true;
                index += 1;
            }
            "--tools" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --tools".to_string())?;
                tools_filter = Some(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect(),
                );
                index += 2;
            }
            flag if flag.starts_with("--tools=") => {
                let value = &flag["--tools=".len()..];
                tools_filter = Some(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect(),
                );
                index += 1;
            }
            "--transport" => {
                // WebSocket transport is a v2.4+ feature.  Accept the flag so
                // callers don't get a hard error, but log a warning and ignore.
                let transport = args.get(index + 1).map(String::as_str).unwrap_or("?");
                if transport != "stdio" {
                    eprintln!(
                        "warning: --transport {transport} is not supported in this version \
                         (only stdio is available); WebSocket transport is planned for v2.4"
                    );
                }
                index += 2;
            }
            flag if flag.starts_with("--transport=") => {
                let transport = &flag["--transport=".len()..];
                if transport != "stdio" {
                    eprintln!(
                        "warning: --transport {transport} is not supported in this version \
                         (only stdio is available); WebSocket transport is planned for v2.4"
                    );
                }
                index += 1;
            }
            other => {
                return Err(format!("unknown argument for mcp-server: {other}"));
            }
        }
    }
    Ok(CliAction::McpServer { read_only, tools_filter })
}

fn parse_direct_slash_cli_action(rest: &[String]) -> Result<CliAction, String> {
    let raw = rest.join(" ");
    match SlashCommand::parse(&raw) {
        Some(SlashCommand::Help { .. }) => Ok(CliAction::Help),
        Some(SlashCommand::Agents { args }) => Ok(CliAction::Agents { args }),
        Some(SlashCommand::Skills { args }) => Ok(CliAction::Skills { args }),
        Some(SlashCommand::Agent {
            subcommand: AgentSubcommand::Traits,
        }) => {
            let catalogue = bundled_catalogue();
            println!("{}", format_traits_listing(catalogue));
            std::process::exit(0);
        }
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
    let mut plugin_manager = build_plugin_manager(&cwd, &loader, &runtime_config);
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
        "danger-full-access" | "default" | _ => PermissionMode::DangerFullAccess,
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
    let reference = args
        .first()
        .ok_or_else(|| "missing session reference for --resume (path | ID | name)".to_string())?;
    // T3-J: accept path, session ID, OR friendly name. We preserve the raw
    // reference here as a PathBuf — actual resolution (path | ID | name)
    // happens at load time via session_meta::resolve_reference_extended so
    // unit tests and `--resume foo /cmd` parsing don't depend on the
    // session existing on disk.
    let session_path = PathBuf::from(reference);
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
    // T3-J: if the literal path doesn't exist, treat it as a session ID
    // or friendly name and resolve via the sidecar metadata.
    let resolved_path: PathBuf = if session_path.exists() {
        session_path.to_path_buf()
    } else if let Some(reference) = session_path.to_str() {
        match session_meta::resolve_reference_extended(reference) {
            Ok((_id, p)) => p,
            Err(error) => {
                eprintln!("failed to restore session: {error}");
                std::process::exit(1);
            }
        }
    } else {
        session_path.to_path_buf()
    };
    let session_path = resolved_path.as_path();

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
    // Mid-conversation switches re-read the entire history against the new
    // model (or the new provider), which bypasses prompt caching for the
    // next response. Claude Code shipped a similar warning in v2.1.108 —
    // it's a real footgun on long sessions where the user didn't realize
    // they just paid to re-tokenize everything. Surface it explicitly.
    let mut report = format!(
        "Model updated
  Previous         {previous}
  Current          {next}
  Preserved        {message_count} messages
  Tip              Existing conversation context stayed attached"
    );
    if message_count > 0 {
        report.push_str(
            "\n  Warning          Next response re-reads full history uncached (new model/provider)",
        );
    }
    report
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

/// Scan tool-result blocks for file paths that appear to have been modified.
///
/// Heuristic: any line in a tool-result output that looks like an absolute or
/// relative file path (contains `/` or `\` and a `.` extension) is collected.
fn collect_modified_files(messages: &[runtime::ConversationMessage]) -> Vec<String> {
    use runtime::{ContentBlock, MessageRole};
    use std::collections::BTreeSet;

    let mut seen: BTreeSet<String> = BTreeSet::new();
    for msg in messages {
        if msg.role != MessageRole::Tool {
            continue;
        }
        for block in &msg.blocks {
            if let ContentBlock::ToolResult { output, .. } = block {
                for line in output.lines() {
                    let trimmed = line.trim();
                    // Accept lines that look like file paths but not URLs.
                    if trimmed.len() > 3
                        && (trimmed.contains('/') || trimmed.contains('\\'))
                        && trimmed.contains('.')
                        && !trimmed.starts_with("http")
                    {
                        // Take only the first token so we don't pick up prose.
                        let candidate = trimmed.split_whitespace().next().unwrap_or(trimmed);
                        if candidate.len() < 256 {
                            seen.insert(candidate.to_owned());
                        }
                    }
                }
            }
        }
    }
    seen.into_iter().take(50).collect()
}

/// Convert a Unix timestamp (seconds) to a human-readable date string.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
fn format_unix_timestamp(secs: u64) -> String {
    // Simple manual formatting: days-since-epoch → year/month/day.
    // Using the proleptic Gregorian calendar algorithm.
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;

    // Algorithm from https://en.wikipedia.org/wiki/Julian_day#Julian_date_calculation
    let z = days + 2_440_588; // Unix epoch = JDN 2440588
    let a = (z as f64 - 1_867_216.25) / 36524.25;
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
        SlashCommand::Help { command } => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(if let Some(cmd) = command {
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
        SlashCommand::Clear { confirm, all_tabs: _ } => {
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
        SlashCommand::Memory { .. } => Ok(ResumeCommandOutcome {
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
        SlashCommand::Export { format, path } => {
            let export_path = resolve_export_path(path.as_deref(), session)?;
            let content = if format.as_deref() == Some("md") {
                render_export_markdown(session)
            } else {
                render_export_text(session)
            };
            fs::write(&export_path, content)?;
            let fmt_label = if format.as_deref() == Some("md") { "markdown" } else { "text" };
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format!(
                    "Export\n  Result           wrote {fmt_label} transcript\n  File             {}\n  Messages         {}",
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
        | SlashCommand::Doctor { .. }
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
        | SlashCommand::RemoteControl { .. }
        | SlashCommand::Loop { .. }
        | SlashCommand::Focus
        | SlashCommand::Mcp { .. }
        | SlashCommand::Productivity
        | SlashCommand::Knowledge { .. }
        | SlashCommand::Daily { .. }
        // v2.2.6 ghost commands — now real handlers in TUI/CLI paths
        | SlashCommand::Tab { .. }
        | SlashCommand::Fork
        | SlashCommand::Share { .. }
        | SlashCommand::Audit
        | SlashCommand::Restart { .. }
        | SlashCommand::Agent { .. }
        | SlashCommand::OutputStyle { .. }
        | SlashCommand::Effort { .. }
        | SlashCommand::Skill { .. }
        | SlashCommand::Goal { .. }
        | SlashCommand::FileCache { .. }
        | SlashCommand::CmdCache { .. }
        | SlashCommand::Profile { .. }
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
#[allow(clippy::single_match_else)]
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
        if let Some(secret) = val.as_str()
            && runtime::vault_session_upsert(key, secret).is_ok() {
                count += 1;
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
        if let Some(val) = runtime::vault_session_get(cred_label)
            && !val.is_empty() {
                unsafe { std::env::set_var(env_var, &val); }
                continue;
            }
        // Plaintext fallback.
        if let Ok(creds_path) = runtime::credentials_path()
            && let Ok(data) = fs::read_to_string(&creds_path)
                && let Ok(root) =
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data)
                    && let Some(val) = root.get(cred_label).and_then(|v| v.as_str())
                        && !val.is_empty() {
                            unsafe { std::env::set_var(env_var, val); }
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

    // Enforce the admin requirements policy (requirements.toml) before
    // entering the REPL.  A violation is a hard error: print it to stderr
    // and exit with code 1.  A missing or malformed policy file is silently
    // ignored (see requirements::load_from_paths).
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    if let Err(PolicyCheckError::Violations(violations)) = loader.load_checked() {
        for v in &violations {
            eprintln!("{}", v.render());
        }
        std::process::exit(1);
    }

    let cli = LiveCli::new(model, true, allowed_tools, permission_mode)?;

    // Use the full-screen TUI only when stdout is an actual terminal.
    let result = if io::stdout().is_terminal() {
        run_repl_tui(cli)
    } else {
        run_repl_plain(cli)
    };

    // Flush any buffered OTel spans before the process exits.
    runtime::otel::shutdown();

    result
}

/// Full-screen TUI REPL loop.
fn run_repl_tui(mut cli: LiveCli) -> Result<(), Box<dyn std::error::Error>> {
    // Cache Ollama models once at startup (non-blocking for tab completions)
    tui::init_ollama_model_cache();

    let (mut tui, sender) =
        AnvilTui::new(cli.model.clone(), cli.session_id(), cli.permission_mode.as_str())?;

    // Install the TUI sender so all model/tool output is routed to it.
    cli.enable_tui(sender);

    // Sync thinking mode and effort level to TUI status bar.
    tui.set_thinking_enabled(cli.thinking_enabled);
    tui.set_effort_level(cli.effort_level.as_str());

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
            if let Some(msg) = check_for_update(&current_version)
                && let Ok(mut slot) = update_slot.lock() {
                    *slot = Some(msg);
                }
        });
    }

    let mut task_check_instant = Instant::now();
    // ── Screensaver state ──────────────────────────────────────────────────────
    let mut screensaver_state: Option<screensaver::FurnaceScreensaver> = None;
    let mut last_input_time = Instant::now();

    // v2.2.11: fire SessionStart hooks after config + MCP loaded, before first prompt.
    {
        let msgs = cli.runtime.run_session_start_hooks();
        for msg in msgs {
            tui.push_system(format!("[hook] {msg}"));
        }
    }

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
        if let Ok(mut slot) = update_check.try_lock()
            && let Some(msg) = slot.take() {
                tui.set_update_available(msg);
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
                    cli.record_daily();
                    break 'outer;
                }
                _ => continue,
            }
        }

        // Drain any queued TUI events (e.g. from previous turn).
        tui.poll_events();

        // Check for messages from remote control web clients.
        {
            let mut remote_messages = Vec::new();
            if let Some(rx) = &cli.relay_input_rx {
                while let Ok(msg) = rx.try_recv() {
                    remote_messages.push(msg);
                }
            }
            for (_tab_id, message) in remote_messages {
                // Handle client connect/disconnect signals from the relay
                if let Some(count_str) = message.strip_prefix("__client_connected:") {
                    let count: usize = count_str.parse().unwrap_or(1);
                    if let Some(session) = &cli.relay_session {
                        let clients = format!("{count} client{}", if count == 1 { "" } else { "s" });
                        tui.set_remote_status(&session.pairing_code, &clients);
                    }
                    tui.push_system(format!("[Remote] Client connected ({count} active)"));
                    // Phase 3: send ConfigSnapshot + VaultState immediately after pairing.
                    {
                        let data = cli.build_configure_data();
                        let config_json = config_data_to_json(&data);
                        let vault_locked = !runtime::vault_is_session_unlocked();
                        if let Some(tx) = &cli.relay_event_tx {
                            let _ = tx.send(runtime::relay::RelayMessage::ConfigSnapshot {
                                config: config_json,
                            });
                            let _ = tx.send(runtime::relay::RelayMessage::VaultState {
                                locked: vault_locked,
                            });
                        }
                    }
                    continue;
                }
                if let Some(count_str) = message.strip_prefix("__client_disconnected:") {
                    let count: usize = count_str.parse().unwrap_or(0);
                    if let Some(session) = &cli.relay_session {
                        if count == 0 {
                            tui.set_remote_status(&session.pairing_code, "0 clients");
                            tui.push_system("[Remote] All clients disconnected".to_string());
                        } else {
                            let clients = format!("{count} client{}", if count == 1 { "" } else { "s" });
                            tui.set_remote_status(&session.pairing_code, &clients);
                            tui.push_system(format!("[Remote] Client disconnected ({count} remaining)"));
                        }
                    }
                    continue;
                }
                // Handle special relay commands
                if let Some(tab_id_str) = message.strip_prefix("__close_tab:") {
                    if let Ok(tab_id) = tab_id_str.parse::<usize>() {
                        if let Some(name) = tui.close_tab_by_id(tab_id) {
                            tui.push_system(format!("[Remote] Closed tab: {name}"));
                            if let Some(tx) = &cli.relay_event_tx {
                                let _ = tx.send(runtime::relay::RelayMessage::TabClosed { tab_id });
                            }
                        } else {
                            tui.push_system(format!("[Remote] Cannot close tab {tab_id} (last tab or not found)"));
                        }
                    }
                    continue;
                }
                if let Some(rest) = message.strip_prefix("__rename_tab:") {
                    if let Some((id_str, new_name)) = rest.split_once(':')
                        && let Ok(tab_id) = id_str.parse::<usize>()
                            && tui.rename_tab_by_id(tab_id, new_name) {
                                tui.push_system(format!("[Remote] Renamed tab to: {new_name}"));
                                if let Some(tx) = &cli.relay_event_tx {
                                    let _ = tx.send(runtime::relay::RelayMessage::TabRenamed {
                                        tab_id,
                                        name: new_name.to_string(),
                                    });
                                }
                            }
                    continue;
                }
                if message == "__config_get" {
                    let data = cli.build_configure_data();
                    let json = config_data_to_json(&data);
                    if let Some(tx) = &cli.relay_event_tx {
                        let _ = tx.send(runtime::relay::RelayMessage::ConfigData { data: json });
                    }
                    continue;
                }
                // JSON config set — for complex payloads like full StatusLineConfig
                if let Some(rest) = message.strip_prefix("__config_set_json:") {
                    if let Some((key, json_str)) = rest.split_once(':')
                        && let Ok(json_value) = serde_json::from_str::<serde_json::Value>(json_str) {
                            let msg = LiveCli::save_anvil_ui_config_key(key, json_value);
                            let success = !msg.contains("error") && !msg.contains("Error");
                            tui.push_system(format!("[Remote Config JSON] {msg}"));
                            if let Some(tx) = &cli.relay_event_tx {
                                let _ = tx.send(runtime::relay::RelayMessage::ConfigUpdated {
                                    key: key.to_string(),
                                    success,
                                    message: msg,
                                });
                            }
                            if success && key == "status_line"
                                && let Ok(config) = serde_json::from_str::<runtime::theme::StatusLineConfig>(json_str) {
                                    tui.set_status_line_config(config);
                                }
                        }
                    continue;
                }
                if let Some(rest) = message.strip_prefix("__config_set:") {
                    if let Some((key, value)) = rest.split_once(':') {
                        let json_value = serde_json::Value::String(value.to_string());
                        let msg = LiveCli::save_anvil_ui_config_key(key, json_value);
                        let success = !msg.contains("error") && !msg.contains("Error");
                        tui.push_system(format!("[Remote Config] {msg}"));
                        if let Some(tx) = &cli.relay_event_tx {
                            let _ = tx.send(runtime::relay::RelayMessage::ConfigUpdated {
                                key: key.to_string(),
                                success,
                                message: msg,
                            });
                        }
                        if success && key == "default_model" {
                            let _ = cli.set_model(Some(value.to_string()));
                        }
                        if success && key == "status_line_preset" {
                            tui.set_status_line_preset(value);
                        }
                    }
                    continue;
                }
                // Phase 3: panel-aware config update (web → host)
                // Format: __config_update:<panel>:<field>:<json_value>
                if let Some(rest) = message.strip_prefix("__config_update:") {
                    // Split into panel, field, value_json — value may contain colons (JSON)
                    let mut parts = rest.splitn(3, ':');
                    let panel = parts.next().unwrap_or("").to_string();
                    let field = parts.next().unwrap_or("").to_string();
                    let value_json = parts.next().unwrap_or("\"\"").to_string();

                    // Vault-sensitive field manifest — these fields require vault unlock to edit.
                    const VAULT_SENSITIVE_FIELDS: &[&str] = &[
                        "anthropic_api_key", "openai_api_key", "xai_api_key", "ollama_api_key",
                        "tavily_api_key", "brave_search_api_key", "exa_api_key",
                        "perplexity_api_key", "google_search_api_key", "bing_search_api_key",
                        "notify_discord_webhook", "notify_slack_webhook", "notify_telegram_token",
                        "notify_matrix_token", "notify_signal_sender",
                        "github_token", "wp_password",
                        "db_url",
                    ];

                    let vault_locked = !runtime::vault_is_session_unlocked();
                    let is_sensitive = VAULT_SENSITIVE_FIELDS.contains(&field.as_str());

                    if vault_locked && is_sensitive {
                        // Vault gate: reject the update and send ConfigError
                        if let Some(tx) = &cli.relay_event_tx {
                            let _ = tx.send(runtime::relay::RelayMessage::ConfigError {
                                panel: panel.clone(),
                                field: field.clone(),
                                message: "Vault is locked — unlock vault to edit sensitive fields".to_string(),
                            });
                        }
                        tui.push_system(format!("[Remote Config] Blocked vault-sensitive field '{field}' while locked"));
                    } else {
                        // Parse the JSON value and apply the config change
                        match serde_json::from_str::<serde_json::Value>(&value_json) {
                            Ok(json_value) => {
                                let msg = LiveCli::save_anvil_ui_config_key(&field, json_value);
                                let success = !msg.to_lowercase().contains("error");
                                tui.push_system(format!("[Remote Config] {msg}"));

                                if success {
                                    // Apply in-session side-effects
                                    if field == "default_model" {
                                        if let Some(model_str) = serde_json::from_str::<serde_json::Value>(&value_json)
                                            .ok().and_then(|v| v.as_str().map(str::to_string))
                                        {
                                            let _ = cli.set_model(Some(model_str));
                                        }
                                    }
                                    if field == "status_line_preset" {
                                        if let Some(preset_str) = serde_json::from_str::<serde_json::Value>(&value_json)
                                            .ok().and_then(|v| v.as_str().map(str::to_string))
                                        {
                                            tui.set_status_line_preset(&preset_str);
                                        }
                                    }
                                    // Phase 3b: full StatusLineConfig live preview
                                    if field == "status_line" {
                                        if let Ok(config) = serde_json::from_str::<runtime::theme::StatusLineConfig>(&value_json) {
                                            tui.set_status_line_config(config);
                                        }
                                    }

                                    // Send updated snapshot as ConfigSaved
                                    let data = cli.build_configure_data();
                                    let config_json = config_data_to_json(&data);
                                    if let Some(tx) = &cli.relay_event_tx {
                                        let _ = tx.send(runtime::relay::RelayMessage::ConfigSaved {
                                            config: config_json,
                                        });
                                    }
                                } else {
                                    // Validation or write error
                                    if let Some(tx) = &cli.relay_event_tx {
                                        let _ = tx.send(runtime::relay::RelayMessage::ConfigError {
                                            panel: panel.clone(),
                                            field: field.clone(),
                                            message: msg,
                                        });
                                    }
                                }
                            }
                            Err(e) => {
                                if let Some(tx) = &cli.relay_event_tx {
                                    let _ = tx.send(runtime::relay::RelayMessage::ConfigError {
                                        panel: panel.clone(),
                                        field: field.clone(),
                                        message: format!("Invalid value JSON: {e}"),
                                    });
                                }
                            }
                        }
                    }
                    continue;
                }

                // Phase 3: vault state request (web → host)
                if message == "__vault_state_get" {
                    let locked = !runtime::vault_is_session_unlocked();
                    if let Some(tx) = &cli.relay_event_tx {
                        let _ = tx.send(runtime::relay::RelayMessage::VaultState { locked });
                    }
                    continue;
                }

                // Phase 4: hub install request from web client
                // Format: __hub_install:<slug>:<version>
                if let Some(rest) = message.strip_prefix("__hub_install:") {
                    let mut parts = rest.splitn(2, ':');
                    let slug = parts.next().unwrap_or("").to_string();
                    let version = parts.next().unwrap_or("").to_string();

                    // Vault gate: refuse while locked
                    if !runtime::vault_is_session_unlocked() {
                        tui.push_system(format!("[Hub] Install blocked: vault locked (slug={slug})"));
                        if let Some(tx) = &cli.relay_event_tx {
                            let _ = tx.send(runtime::relay::RelayMessage::HubInstallError {
                                slug: slug.clone(),
                                reason: "vault_locked".to_string(),
                                message: "Vault is locked — unlock vault to install packages".to_string(),
                            });
                        }
                        continue;
                    }

                    tui.push_system(format!("[Hub] Installing {slug} v{version}..."));

                    // Progress: downloading
                    if let Some(tx) = &cli.relay_event_tx {
                        let _ = tx.send(runtime::relay::RelayMessage::HubInstallProgress {
                            slug: slug.clone(),
                            phase: "downloading".to_string(),
                            percent: 0,
                        });
                    }

                    // Build HubClient and fetch package detail
                    let hub_url = cli.anvil_config_str("anvilhub_url", "https://anvilhub.culpur.net");
                    let install_dir = anvil_home_dir();

                    let (pkg_result, install_result) = {
                        match tokio::runtime::Handle::try_current() {
                            Ok(handle) => {
                                let client = runtime::hub::BlockingHubClient::new(&hub_url, handle);
                                let pkg = client.get_package(&slug);
                                match pkg {
                                    Ok(p) => {
                                        let v = p.version.clone();
                                        let ptype = p.pkg_type.clone();
                                        let result = client.install(&p, &install_dir);
                                        (Ok((v, ptype)), result)
                                    }
                                    Err(e) => (Err(e), Err(runtime::hub::HubError::Install("no pkg".into()))),
                                }
                            }
                            Err(_) => match tokio::runtime::Runtime::new() {
                                Ok(rt) => {
                                    let client = runtime::hub::BlockingHubClient::new(&hub_url, rt.handle().clone());
                                    let pkg = client.get_package(&slug);
                                    match pkg {
                                        Ok(p) => {
                                            let v = p.version.clone();
                                            let ptype = p.pkg_type.clone();
                                            let result = client.install(&p, &install_dir);
                                            (Ok((v, ptype)), result)
                                        }
                                        Err(e) => (Err(e), Err(runtime::hub::HubError::Install("no pkg".into()))),
                                    }
                                }
                                Err(e) => {
                                    let err_msg = format!("could not start async runtime: {e}");
                                    if let Some(tx) = &cli.relay_event_tx {
                                        let _ = tx.send(runtime::relay::RelayMessage::HubInstallError {
                                            slug: slug.clone(),
                                            reason: "runtime_error".to_string(),
                                            message: err_msg.clone(),
                                        });
                                    }
                                    tui.push_system(format!("[Hub] Install failed: {err_msg}"));
                                    continue;
                                }
                            },
                        }
                    };

                    match pkg_result {
                        Err(e) => {
                            let (reason, msg) = match &e {
                                runtime::hub::HubError::NotFound(_) => ("not_found", format!("Package '{slug}' not found on AnvilHub")),
                                runtime::hub::HubError::Http(m) => ("network_error", format!("AnvilHub is unreachable: {m}")),
                                other => ("api_error", format!("{other}")),
                            };
                            tui.push_system(format!("[Hub] Install failed: {msg}"));
                            if let Some(tx) = &cli.relay_event_tx {
                                let _ = tx.send(runtime::relay::RelayMessage::HubInstallError {
                                    slug: slug.clone(),
                                    reason: reason.to_string(),
                                    message: msg,
                                });
                            }
                        }
                        Ok((resolved_version, pkg_type)) => {
                            match install_result {
                                Err(e) => {
                                    let (reason, msg) = match &e {
                                        runtime::hub::HubError::Install(m) => ("install_failed", format!("Install failed: {m}")),
                                        other => ("install_failed", format!("{other}")),
                                    };
                                    tui.push_system(format!("[Hub] Install failed: {msg}"));
                                    if let Some(tx) = &cli.relay_event_tx {
                                        let _ = tx.send(runtime::relay::RelayMessage::HubInstallError {
                                            slug: slug.clone(),
                                            reason: reason.to_string(),
                                            message: msg,
                                        });
                                    }
                                }
                                Ok(_dest) => {
                                    // Determine RestartRequirement from package type
                                    let requires_restart = match pkg_type.as_str() {
                                        "plugin" | "mcp" => "full",
                                        "theme" => "soft",
                                        _ => "none", // skill, agent
                                    };

                                    tui.push_system(format!(
                                        "[Hub] Installed {slug} v{resolved_version} (restart={requires_restart})"
                                    ));

                                    // Fire-and-forget telemetry POST to passage-culpur.net (Phase 4b)
                                    let platform = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                                        "darwin-arm64"
                                    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
                                        "darwin-x86_64"
                                    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
                                        "linux-x86_64"
                                    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
                                        "linux-arm64"
                                    } else if cfg!(target_os = "windows") {
                                        "windows-x86_64"
                                    } else {
                                        "linux-x86_64"
                                    };
                                    let tel_slug = slug.clone();
                                    let tel_version = resolved_version.clone();
                                    let tel_hub_url = hub_url.clone();
                                    // Spawn telemetry as a detached thread — failure is acceptable
                                    std::thread::spawn(move || {
                                        if let Ok(rt) = tokio::runtime::Runtime::new() {
                                            let hub_client = runtime::hub::HubClient::new(&tel_hub_url);
                                            rt.block_on(hub_client.post_install_telemetry(
                                                &tel_slug,
                                                &tel_version,
                                                platform,
                                            ));
                                        }
                                    });

                                    // Broadcast success
                                    if let Some(tx) = &cli.relay_event_tx {
                                        let _ = tx.send(runtime::relay::RelayMessage::HubInstalled {
                                            slug: slug.clone(),
                                            version: resolved_version.clone(),
                                            requires_restart: requires_restart.to_string(),
                                        });
                                    }

                                    // Soft restart: config reload (no respawn needed)
                                    if requires_restart == "soft" {
                                        tui.push_system("[Hub] Theme installed — config reloaded".to_string());
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }

                // Phase 4: web client requests respawn
                if message == "__respawn_request" {
                    tui.push_system("[Hub] Respawn requested from web client".to_string());
                    let ctx = get_respawn_ctx();
                    let session_id = cli.session_id().to_owned();
                    match respawn::respawn(&ctx, "web hub.install restart", &session_id) {
                        Ok(respawn::RespawnOutcome::Respawned) => {
                            // Process replaced — unreachable
                        }
                        Ok(respawn::RespawnOutcome::PromptUser(msg)) => {
                            tui.push_system(format!("[Restart] {msg}"));
                            if let Some(tx) = &cli.relay_event_tx {
                                let _ = tx.send(runtime::relay::RelayMessage::System {
                                    tab_id: 0,
                                    message: format!("[Restart] {msg}"),
                                });
                            }
                        }
                        Err(e) => {
                            tui.push_system(format!("[Restart] Respawn failed: {e}"));
                        }
                    }
                    continue;
                }

                if let Some(tab_name) = message.strip_prefix("__new_tab:") {
                    let new_session = create_managed_session_handle()?;
                    let tab_idx = tui.new_tab(tab_name, cli.model.clone(), new_session.id.clone());
                    tui.switch_tab(tab_idx);
                    tui.push_system(format!("[Remote] Opened tab: {tab_name}"));
                    // Broadcast tab_opened to relay so web viewer adds the tab
                    if let Some(tx) = &cli.relay_event_tx {
                        let _ = tx.send(runtime::relay::RelayMessage::TabOpened {
                            tab_id: tab_idx,
                            name: tab_name.to_string(),
                            model: cli.model.clone(),
                            session_id: new_session.id.clone(),
                        });
                    }
                    continue;
                }

                // Check if the remote message is a slash command
                if message.starts_with('/') {
                    // Vault commands: run silently via JSON API, send structured result to web viewer
                    if message.starts_with("/vault ") {
                        let vault_args = message.strip_prefix("/vault ").unwrap_or("").trim();
                        // Parse: /vault <operation> [password] [args...]
                        // For web viewer, format is: /vault list <pw> or /vault get <label> <pw> or /vault store <label> <secret> <pw>
                        let mut parts = vault_args.splitn(2, ' ');
                        let operation = parts.next().unwrap_or("");
                        let rest = parts.next().unwrap_or("");

                        // Extract password (last space-separated token for list/unlock, or handled differently for get/store/delete)
                        let (password, arg) = match operation {
                            "list" | "unlock" | "lock" | "scan" => (rest.to_string(), String::new()),
                            "get" | "delete" => {
                                // /vault get <label> <pw>
                                let mut p = rest.rsplitn(2, ' ');
                                let pw = p.next().unwrap_or("").to_string();
                                let label = p.next().unwrap_or("").to_string();
                                (pw, label)
                            }
                            "store" => {
                                // /vault store <label> <secret> <pw>  — pw is last token
                                let mut p = rest.rsplitn(2, ' ');
                                let pw = p.next().unwrap_or("").to_string();
                                let label_secret = p.next().unwrap_or("").to_string();
                                (pw, label_secret)
                            }
                            _ => (String::new(), rest.to_string()),
                        };

                        if let Some(tx) = &cli.relay_event_tx {
                            if operation == "lock" {
                                let _ = tx.send(runtime::relay::RelayMessage::ConfigData {
                                    data: serde_json::json!({"vault_op": "lock", "success": true}),
                                });
                            } else if operation == "scan" {
                                let result = crate::vault::run_vault_command_impl(Some("scan"));
                                let _ = tx.send(runtime::relay::RelayMessage::ConfigData {
                                    data: serde_json::json!({"vault_result": result}),
                                });
                            } else {
                                let result = crate::vault::vault_json_operation(&password, operation, &arg);
                                let _ = tx.send(runtime::relay::RelayMessage::ConfigData {
                                    data: serde_json::json!({"vault_op": operation, "vault_data": result}),
                                });
                            }
                        }
                        continue;
                    }

                    if let Some(command) = SlashCommand::parse(&message) {
                        tui.push_system(format!("[Remote] {message}"));
                        match cli.handle_repl_command_tui(command, &mut tui) {
                            Ok(_) => {}
                            Err(err) => {
                                tui.push_system(format!("Error: {err}"));
                            }
                        }
                        tui.set_thinking_enabled(cli.thinking_enabled);
                        tui.set_effort_level(cli.effort_level.as_str());
                        continue;
                    }
                }

                tui.push_system(format!("[Remote] {message}"));
                tui.scroll_to_bottom();
                tui.draw()?;
                let cli_ref = &mut cli;
                let input_owned = message;
                let turn_err: std::sync::Arc<std::sync::Mutex<Option<String>>> =
                    std::sync::Arc::new(std::sync::Mutex::new(None));
                let turn_err_clone = turn_err.clone();
                std::thread::scope(|s| {
                    s.spawn(move || {
                        if let Err(e) = cli_ref.run_turn(&input_owned)
                            && let Ok(mut guard) = turn_err_clone.lock() {
                                *guard = Some(e.to_string());
                            }
                    });
                    let _ = tui.wait_for_turn_end();
                });
                if let Some(err) = turn_err.lock().ok().and_then(|g| g.clone()) {
                    tui.push_system(format!("Turn error: {err}"));
                }
            }
        }

        // Read the next key event (returns quickly with Continue most of the time).
        match tui.read_input()? {
            ReadResult::Continue => {
                // Nothing submitted yet; loop and redraw.
            }
            ReadResult::Exit => {
                cli.persist_session()?;
                cli.record_daily();
                break 'outer;
            }
            ReadResult::ConfigureAction(action) => {
                // Apply status line preset changes immediately to the TUI.
                if let ConfigureAction::SetStatusLinePreset { ref preset } = action {
                    tui.set_status_line_preset(preset);
                }
                if let ConfigureAction::ApplyStatusLineConfig { ref config } = action {
                    tui.set_status_line_config(*config.clone());
                }
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
                    cli.record_daily();
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
                            let closed_idx = tui.active_tab_index();
                            if let Some(name) = tui.close_active_tab() {
                                tui.push_system(format!("Closed tab: {name}"));
                                if let Some(tx) = &cli.relay_event_tx {
                                    let _ = tx.send(runtime::relay::RelayMessage::TabClosed { tab_id: closed_idx });
                                }
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
                        "" => {
                            tui.push_system(
                                "Tab commands:\n  /tab new [name]     open a new tab\n  /tab close          close current tab\n  /tab list           list all tabs\n  /tab rename <name>  rename current tab\n  /tab <N>            switch to tab N\n\nKey bindings:\n  Ctrl+T              new tab\n  Ctrl+W              close tab\n  Ctrl+Left/Right     previous / next tab\n  Alt+1..9            switch to tab N".to_string(),
                            );
                        }
                        n => {
                            if let Ok(num) = n.parse::<usize>() {
                                tui.switch_tab(num.saturating_sub(1));
                            } else {
                                tui.push_system(format!("Unknown /tab action: {n}. Try /tab for help."));
                            }
                        }
                    }
                    continue;
                }

                // /fork is TUI-only — conversation branching
                if let Some(fork_rest) = trimmed.strip_prefix("/fork") {
                    let rest = fork_rest.trim();
                    let parts: Vec<&str> = rest.splitn(2, ' ').collect();
                    let action = parts.first().copied().unwrap_or("").trim();
                    match action {
                        "list" | "ls" => {
                            let branches = tui.active_tab().branches.iter().enumerate()
                                .map(|(i, (name, log))| {
                                    let active = if i + 1 == tui.active_tab().active_branch { " (active)" } else { "" };
                                    format!("  {}. {} — {} messages{}", i + 1, name, log.len(), active)
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            if branches.is_empty() {
                                tui.push_system("No branches yet. Use /fork [name] to create one.".to_string());
                            } else {
                                tui.push_system(format!("Conversation branches:\n{branches}"));
                            }
                        }
                        "switch" => {
                            if let Some(idx_str) = parts.get(1) {
                                if let Ok(idx) = idx_str.trim().parse::<usize>() {
                                    if tui.active_tab_mut().switch_branch(idx) {
                                        tui.push_system(format!("Switched to branch {idx}"));
                                    } else {
                                        tui.push_system(format!("Invalid branch: {idx}. Use /fork list to see branches."));
                                    }
                                } else {
                                    tui.push_system("Usage: /fork switch <number>".to_string());
                                }
                            } else {
                                tui.push_system("Usage: /fork switch <number>".to_string());
                            }
                        }
                        "" => {
                            tui.push_system(
                                "Fork commands:\n  /fork [name]        create a branch from current conversation\n  /fork list           list all branches\n  /fork switch <N>     switch to branch N".to_string(),
                            );
                        }
                        name => {
                            let idx = tui.active_tab_mut().create_branch(name);
                            tui.push_system(format!("Branch '{name}' created (#{idx}) — current conversation preserved as branch point"));
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
                    // Sync thinking mode and effort level to status bar after any command.
                    tui.set_thinking_enabled(cli.thinking_enabled);
                    tui.set_effort_level(cli.effort_level.as_str());
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
                        tui.scroll_to_bottom();
                        tui.draw()?;
                        let cli_ref = &mut cli;
                        let turn_err: std::sync::Arc<std::sync::Mutex<Option<String>>> =
                            std::sync::Arc::new(std::sync::Mutex::new(None));
                        let turn_err_clone = turn_err.clone();
                        std::thread::scope(|s| {
                            s.spawn(move || {
                                if let Err(e) = cli_ref.run_turn_file_drop()
                                    && let Ok(mut guard) = turn_err_clone.lock() {
                                        *guard = Some(e.to_string());
                                    }
                            });
                            let _ = tui.wait_for_turn_end();
                        });
                        if let Some(err) = turn_err.lock().ok().and_then(|g| g.clone()) {
                            tui.push_system(format!("Turn error: {err}"));
                        }
                    }
                    continue;
                }

                // User prompt: run a turn.  The TUI sender is already installed,
                // so model output streams back to the TUI.
                tui.push_system(format!("Thinking... ({})", cli.model));
                tui.scroll_to_bottom();
                tui.draw()?; // Immediate visual feedback before blocking API call

                // Run the turn on a scoped background thread so the main
                // thread can enter the event-drain / animation loop immediately.
                // This enables live streaming display instead of buffering
                // the full response before showing anything.
                //
                // T1-#400: wait_for_turn_end now ALSO polls keyboard input
                // during the wait, so the user can compose the next prompt
                // while the current turn streams. If they pressed Enter
                // mid-turn, the captured draft is returned here as
                // `held_submit` and we feed it back into the input loop so
                // it auto-submits as the next turn (no user action needed).
                let mut held_submit: Option<String> = None;
                {
                    let cli_ref = &mut cli;
                    let input_owned = trimmed.to_string();
                    let turn_err: std::sync::Arc<std::sync::Mutex<Option<String>>> =
                        std::sync::Arc::new(std::sync::Mutex::new(None));
                    let turn_err_clone = turn_err.clone();
                    std::thread::scope(|s| {
                        s.spawn(move || {
                            if let Err(e) = cli_ref.run_turn(&input_owned)
                                && let Ok(mut guard) = turn_err_clone.lock() {
                                    *guard = Some(e.to_string());
                                }
                        });
                        // Main thread: animate the TUI + accept in-flight typing.
                        if let Ok(captured) = tui.wait_for_turn_end() {
                            held_submit = captured;
                        }
                    });
                    if let Some(err) = turn_err.lock().ok().and_then(|g| g.clone()) {
                        tui.push_system(format!("Turn error: {err}"));
                    }
                }
                // T1-#400: if user pressed Enter during the in-flight turn,
                // surface the held draft as a system note and stash it in
                // the input box so the NEXT iteration of the read_input loop
                // sees it.  We don't auto-fire to keep cancel-on-misfire
                // possible — the user can press Enter to ship it or edit
                // first.  The held draft is already trimmed.
                if let Some(draft) = held_submit
                    && !draft.is_empty()
                {
                    tui.push_system(format!("↳ resuming with held draft: {draft}"));
                    tui.set_input(draft);
                }
                // Update footer QMD/archive status after each turn
                if cli.qmd.is_enabled()
                    && let Some(status) = cli.qmd.status() {
                        tui.set_qmd_status(format!(
                            "QMD: {} docs, {} vecs",
                            status.total_docs, status.total_vectors
                        ));
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

    // v2.2.11: fire SessionEnd hooks on clean exit.
    let _ = cli.runtime.run_session_end_hooks();

    // Capture the session id BEFORE we drop `tui` (and thus `cli`).
    let exit_session_id = cli.session_id().to_string();

    // Drop `tui` here — the Drop impl restores the terminal.
    drop(tui);

    // T3-Exit-UX: print resume-friendly exit message AFTER the alternate
    // screen has been torn down, so the lines persist in the user's
    // normal scrollback rather than disappearing with the TUI.
    print_exit_resume_banner(&exit_session_id);

    Ok(())
}

/// Print the post-exit "session saved + resume command" block.
///
/// Format (per feedback-anvil-exit-resume-ux memory):
///   Session saved as 'auth-refactor' (id: session-1778365293)
///     ↻  anvil --continue            # resume this session
///     ↻  anvil --resume auth-refactor # or by name/id
///
/// If the session has no friendly name (no sidecar), we omit the name
/// clause and only show the id-based --resume invocation.
fn print_exit_resume_banner(session_id: &str) {
    let name = session_meta::get_session_name(session_id);
    if let Some(ref n) = name {
        println!("Session saved as '{n}' (id: {session_id})");
    } else {
        println!("Session saved (id: {session_id})");
    }
    println!("  ↻  anvil --continue            # resume this session");
    if let Some(ref n) = name {
        println!("  ↻  anvil --resume {n}  # or by name/id");
    } else {
        println!("  ↻  anvil --resume {session_id}  # or by id");
    }
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
                    cli.record_daily();
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
                cli.record_daily();
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
    /// Output style: Precise (default) or Condensed (prepends terse skill body).
    output_style: OutputStyle,
    /// Per-session effort/reasoning level for API calls.
    effort_level: EffortLevel,
    /// Sub-agent manager — tracks spawned agents and their status.
    /// Wrapped in Arc<Mutex<>> so it can be shared with `CliToolExecutor`.
    agent_manager: Arc<Mutex<agents::AgentManager>>,
    /// Active relay session (present while `/remote-control` is running).
    relay_session: Option<runtime::relay::RelaySession>,
    /// Broadcast sender for relay events (present while a relay session is active).
    relay_event_tx: Option<tokio::sync::broadcast::Sender<runtime::relay::RelayMessage>>,
    /// Receiver for messages from remote control web clients.
    relay_input_rx: Option<std::sync::mpsc::Receiver<(usize, String)>>,
    /// Manager for ephemeral read-only share URLs (`/share` command).
    share_manager: share::ShareManager,
    /// Wall-clock start time of this session (used by daily summaries).
    session_start: Instant,
    /// T4-O: per-turn mtime snapshot of ANVIL.md / MEMORY.md candidates.
    /// Populated lazily on first turn; on each subsequent turn we re-stat
    /// the same paths and rebuild the system prompt if any mtime changed.
    instructions_mtime: std::collections::HashMap<PathBuf, std::time::SystemTime>,
}

impl LiveCli {
    fn new(
        model: String,
        enable_tools: bool,
        allowed_tools: Option<AllowedToolSet>,
        permission_mode: PermissionMode,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let provider = friendly_provider_label(&model);
        let system_prompt = build_system_prompt_with_identity(
            Some(model.clone()),
            provider,
            None,
        )?;
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
            output_style: load_output_style(),
            effort_level: runtime::resolve_effort(None, None),
            relay_session: None,
            relay_event_tx: None,
            relay_input_rx: None,
            share_manager: share::ShareManager::new(),
            session_start: Instant::now(),
            instructions_mtime: std::collections::HashMap::new(),
        };
        cli.persist_session()?;
        // Emit session_start OTel event (no-op when disabled).
        {
            let provider_name = match api::detect_provider_kind(&cli.model) {
                api::ProviderKind::AnvilApi => "anthropic",
                api::ProviderKind::Xai => "xai",
                api::ProviderKind::OpenAi => "openai",
                api::ProviderKind::Gemini => "gemini",
                api::ProviderKind::Ollama => "ollama",
            };
            runtime::otel::session_start(
                &cli.session.id,
                &cli.model,
                provider_name,
                "normal",
                "default",
            );
        }
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

        if let Some(tx) = tui_tx {
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
        // T4-O: hot-reload ANVIL.md / MEMORY.md if either changed since the
        // last turn. Cheap (per-turn stat of <10 paths) and silent when
        // nothing has changed. We rebuild the system prompt in place — any
        // user-set output style or fast-mode prefix is re-applied below by
        // the existing run_turn flow on the *next* turn, but the worst case
        // is that the prefix gets dropped for one turn.
        if self.maybe_reload_instructions() {
            self.runtime.replace_system_prompt(self.system_prompt.clone());
        }

        // Inject any pinned files at the start of each turn.
        if let Ok(pinned_path) = anvil_pinned_path()
            && let Ok(pinned) = load_pinned_paths(&pinned_path) {
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

        // Apply the session effort level to the environment so that provider
        // wire-body builders (and any child processes / MCP servers spawned
        // during this turn) inherit the active setting.
        self.effort_level.apply_to_env();

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

        if let Some(tx) = tui_tx {
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
                    // Fire Notification(completion) hook after turn completes.
                    self.runtime.run_notification_hooks(&NotificationPayload {
                        kind: NotificationKind::Completion,
                        message: format!(
                            "Turn complete ({} input / {} output tokens)",
                            usage.input_tokens, usage.output_tokens
                        ),
                    });
                    self.persist_session()?;
                    // Check whether we should auto-compact and archive.
                    if let Some(msg) = self.maybe_auto_compact() {
                        tx.send(TuiEvent::System(msg));
                    }
                    // Emit skill suggestion hint as a TUI system message (non-blocking).
                    {
                        use commands::agents::{discover_skill_roots, load_skills_from_roots};
                        use commands::{ChainEvaluator, format_chain_hint};
                        let cwd = std::env::current_dir().unwrap_or_default();
                        let roots = discover_skill_roots(&cwd);
                        if let Ok(skills) = load_skills_from_roots(&roots) {
                            // Trigger-based suggestion hint.
                            let skills_with_triggers: Vec<&commands::agents::SkillSummary> =
                                skills.iter().filter(|s| !s.triggers.is_empty()).collect();
                            let matches = match_triggers(input, &skills_with_triggers);
                            if let Some(hint) = format_suggestions_hint(&matches) {
                                tx.send(TuiEvent::System(hint));
                            }
                            // Chain-based suggestion hint for loaded skills.
                            let loaded: Vec<commands::agents::SkillSummary> = self.loaded_skills_snapshot();
                            if !loaded.is_empty() {
                                let all_skills: std::collections::HashMap<String, commands::agents::SkillSummary> =
                                    skills.into_iter().map(|s| (s.name.to_ascii_lowercase(), s)).collect();
                                let evaluator = ChainEvaluator::new();
                                let candidates = evaluator.evaluate(&loaded, &all_skills, input);
                                for loaded_skill in &loaded {
                                    let skill_candidates: Vec<_> = candidates.iter()
                                        .filter(|c| c.triggered_by.eq_ignore_ascii_case(&loaded_skill.name))
                                        .cloned()
                                        .collect();
                                    if let Some(hint) = format_chain_hint(&loaded_skill.name, &skill_candidates) {
                                        tx.send(TuiEvent::System(hint));
                                    }
                                }
                            }
                        }
                    }
                    Ok(())
                }
                Err(error) => {
                    let err_msg = format!("Error: {error}");
                    // Fire Notification(error) hook before displaying the error.
                    self.runtime.run_notification_hooks(&NotificationPayload {
                        kind: NotificationKind::Error,
                        message: err_msg.clone(),
                    });
                    tx.send(TuiEvent::System(err_msg));
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
                    // Emit skill suggestion hint as turn-end footer (non-blocking).
                    self.maybe_emit_skill_hint(input);
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
            eprintln!("QMD is not available — ensure `qmd` is installed and on your PATH.");
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
                let new_model = model.as_deref().unwrap();
                let previous = self.model.clone();
                self.model = new_model.to_string();
                let msg_count = self.runtime.session().messages.len();
                tui.set_model(self.model.clone());
                tui.push_system(format_model_switch_report(&previous, &self.model, msg_count));
                return Ok(false);
            }
            SlashCommand::GenerateImage { prompt, wp_post_id } => {
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
            SlashCommand::RemoteControl { action } => {
                let msg = self.run_remote_control_command(action.as_deref());
                tui.push_system(msg);
                // Wire the relay broadcast channel into the TUI for event forwarding
                if let Some(tx) = &self.relay_event_tx {
                    tui.set_relay_tx(tx.clone());
                    // Send session metadata so the web viewer has full context
                    let _ = tx.send(runtime::relay::RelayMessage::SessionMeta {
                        session_id: self.session_id().to_string(),
                        model: self.model.clone(),
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        permission_mode: self.permission_mode.as_str().to_string(),
                        thinking_enabled: self.thinking_enabled,
                        qmd_status: if self.qmd.is_enabled() {
                            self.qmd.status().map(|s| format!("{} docs, {} vectors", s.total_docs, s.total_vectors))
                        } else {
                            None
                        },
                        block_time: None,
                        status_line_preset: Some(tui.status_line_config().preset.clone()),
                    });
                    // Broadcast existing tabs so the viewer knows what tabs exist
                    for (idx, name, model, session_id) in tui.tab_details() {
                        let _ = tx.send(runtime::relay::RelayMessage::TabOpened {
                            tab_id: idx,
                            name,
                            model,
                            session_id,
                        });
                    }
                } else {
                    tui.clear_relay_tx();
                }
                // Update TUI status bar — must be after all relay setup
                if let Some(s) = &self.relay_session {
                    tui.set_remote_status(&s.pairing_code, "waiting");
                } else {
                    tui.clear_remote_status();
                }
                // Force immediate redraw so the status bar updates visually
                let _ = tui.draw();
                return Ok(false);
            }
            SlashCommand::Focus => {
                tui.focus_mode = !tui.focus_mode;
                tui.push_system(if tui.focus_mode {
                    "Focus view enabled — showing prompts, tool summaries, and responses only".to_string()
                } else {
                    "Focus view disabled — showing full conversation".to_string()
                });
                return Ok(false);
            }
            SlashCommand::Loop { prompt } => {
                tui.push_system(format!(
                    "Loop mode: {}",
                    if let Some(p) = prompt { format!("will repeat: {p}") } else { "use /loop <prompt> to set a recurring prompt".to_string() }
                ));
                return Ok(false);
            }
            SlashCommand::Share { action } => {
                // Vault gate — sharing requires an unlocked vault (anti-abuse).
                if !runtime::vault_is_session_unlocked() {
                    tui.push_system(
                        "This command requires the vault to be unlocked. Run /vault unlock first.".to_string()
                    );
                    return Ok(false);
                }
                let msg = match action.as_deref().unwrap_or("").trim() {
                    "stop" => {
                        let tab = tui.active_tab();
                        self.share_manager.stop_share(tab)
                    }
                    "list" => self.share_manager.list_shares(),
                    // No subcommand or unrecognised → share the current tab.
                    _ => {
                        let tab = tui.active_tab();
                        // share_tab takes &Tab, so snapshot the needed fields
                        self.share_manager.share_tab(tab)
                    }
                };
                tui.push_system(msg);
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
            SlashCommand::Help { command } => {
                let text = if let Some(cmd) = command.as_deref() {
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
            SlashCommand::Memory { .. } => {
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
            SlashCommand::Agents { args } => {
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
            SlashCommand::OutputStyle { style } => {
                let msg = self.set_output_style(style).unwrap_or_else(|e| e.to_string());
                (msg, false)
            }
            SlashCommand::Effort { level } => {
                let msg = self.set_effort(level);
                (msg, false)
            }
            SlashCommand::Clear { confirm, all_tabs } => {
                let changed = self.clear_session(confirm)?;
                if changed {
                    // T4-N: tell the TUI to wipe its visible display state so
                    // the user no longer sees the just-cleared session.
                    if let Some(tx) = self.tui_slot.lock().ok().and_then(|g| g.clone()) {
                        tx.send(TuiEvent::WorkspaceClear { all_tabs });
                    }
                    let msg = if all_tabs {
                        "Workspace cleared (all tabs).".to_string()
                    } else {
                        "Session cleared.".to_string()
                    };
                    (msg, true)
                } else {
                    ("Use /clear --confirm to clear.".to_string(), false)
                }
            }
            SlashCommand::Init => {
                run_init()?;
                ("Initialized ANVIL.md and config files.".to_string(), false)
            }
            SlashCommand::Export { format, path } => {
                self.export_session(format.as_deref(), path.as_deref())?;
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
            SlashCommand::Doctor { mode } => {
                let out = match mode.as_deref() {
                    Some("release") => Self::run_release_doctor(),
                    _ => self.run_doctor(),
                };
                (out, false)
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
            // T5-Ssh: embedded SSH client (Phase A — parser only; full wiring in Phase F)
            // Supersedes the prior `/ssh` session-manager stub; the new contract
            // delivers a real interactive SSH tab inside Anvil.
            SlashCommand::Ssh { args: _ } => {
                ("/ssh: embedded SSH client not yet wired (Phase F).".to_string(), false)
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
            // ---------------------------------------------------------------
            // Phase 1: 8 previously-missing TUI handlers
            // ---------------------------------------------------------------
            SlashCommand::Mcp { action } => {
                (self.run_mcp_command(action.as_deref()), false)
            }
            SlashCommand::Plugins { action, target } => {
                self.handle_plugins_command_tui(action.as_deref(), target.as_deref())?
            }
            SlashCommand::Session { action, target } => {
                self.handle_session_command_tui(action.as_deref(), target.as_deref())?
            }
            SlashCommand::Resume { session_path } => {
                self.resume_session_tui(session_path)?
            }
            SlashCommand::Sleep => {
                // Sleep is intercepted earlier in the event loop (line ~1958) before
                // SlashCommand::parse() is called, so this arm is never reached in TUI.
                // It is here solely for exhaustiveness. Return a hint just in case.
                ("Sleep/screensaver activated.".to_string(), false)
            }
            SlashCommand::Productivity => {
                (self.run_productivity_command(), false)
            }
            SlashCommand::Knowledge { action } => {
                (self.run_knowledge_command(action.as_deref()), false)
            }
            SlashCommand::Daily { date } => {
                (self.run_daily_command(date.as_deref()), false)
            }
            // ---------------------------------------------------------------
            // Phase 1: 4 ghost commands (Tab/Fork intercepted in event loop)
            // ---------------------------------------------------------------
            SlashCommand::Tab { action } => {
                // /tab is intercepted in the main event loop before SlashCommand::parse().
                // This arm exists for exhaustiveness; in practice it is dead code in TUI.
                let action_str = action.as_deref().unwrap_or("list");
                (format!("Tab command '{action_str}' — use Ctrl+T / Ctrl+W / Ctrl+Left/Right or type /tab in the TUI input."), false)
            }
            SlashCommand::Fork => {
                // /fork is intercepted in the main event loop before SlashCommand::parse().
                // This arm exists for exhaustiveness.
                ("Fork command — type /fork [name] directly in the TUI input to branch the conversation.".to_string(), false)
            }
            SlashCommand::Share { action } => {
                // In TUI mode, Share is intercepted in handle_repl_command_tui so it can
                // access the active tab.  This arm exists for compile-time exhaustiveness
                // and is not reached during normal TUI operation.
                let _ = action;
                ("Share command dispatched via TUI handler.".to_string(), false)
            }
            SlashCommand::Audit => {
                // Composite: security scan + deps audit + vault verify, concatenated.
                let security = self.run_security_command(Some("scan"));
                let deps = Self::run_deps_command(Some("audit"));
                let vault = self.run_vault_command(Some("verify"));
                (format!("=== Security Scan ===\n{security}\n\n=== Deps Audit ===\n{deps}\n\n=== Vault Verify ===\n{vault}"), false)
            }
            SlashCommand::Restart { soft } => {
                if soft {
                    // Soft restart: reload config in-place.
                    Self::print_config(None)?;
                    ("Config reloaded.".to_string(), false)
                } else {
                    // Full restart: prompt then respawn (TUI path — prompt via stdout).
                    print!("Save and restart Anvil? [y/N] ");
                    let _ = io::stdout().flush();
                    let mut choice = String::new();
                    let _ = io::stdin().read_line(&mut choice);
                    let answer = choice.trim().to_ascii_lowercase();
                    if answer == "y" || answer == "yes" {
                        let ctx = get_respawn_ctx();
                        let session_id = self.session.id.clone();
                        match respawn::respawn(&ctx, "user /restart", &session_id) {
                            Ok(respawn::RespawnOutcome::Respawned) => {
                                // Unreachable — exec replaced us.
                                (String::new(), false)
                            }
                            Ok(respawn::RespawnOutcome::PromptUser(msg)) => {
                                // Respawn unsafe: print message and exit with code 42.
                                (msg, false)
                            }
                            Err(e) => {
                                (format!("Restart failed: {e}"), false)
                            }
                        }
                    } else {
                        ("Restart cancelled.".to_string(), false)
                    }
                }
            }
            // ---------------------------------------------------------------
            // Exhaustiveness arms for commands intercepted before this function
            // in handle_repl_command_tui. These arms are unreachable in practice
            // but required for the match to compile without a catch-all.
            // ---------------------------------------------------------------
            SlashCommand::Branch { .. } => {
                // In non-TUI path, Branch renders "unavailable" via handle_repl_command.
                // In TUI path this is intercepted above (renders mode_unavailable).
                ("Branch commands require a git-capable terminal session.".to_string(), false)
            }
            SlashCommand::Worktree { .. } => {
                ("Worktree commands require a git-capable terminal session.".to_string(), false)
            }
            SlashCommand::CommitPushPr { .. } => {
                ("commit-push-pr requires a full terminal session.".to_string(), false)
            }
            SlashCommand::Qmd { .. } => {
                // Intercepted in handle_repl_command_tui. This arm is unreachable.
                ("QMD search requires an active QMD session.".to_string(), false)
            }
            SlashCommand::RemoteControl { .. } => {
                // Intercepted in handle_repl_command_tui. This arm is unreachable.
                ("Remote control is handled by the TUI relay.".to_string(), false)
            }
            SlashCommand::Loop { .. } => {
                // Intercepted in handle_repl_command_tui. This arm is unreachable.
                ("Loop mode is handled by the TUI event loop.".to_string(), false)
            }
            SlashCommand::Focus => {
                // Intercepted in handle_repl_command_tui. This arm is unreachable.
                ("Focus mode is handled by the TUI event loop.".to_string(), false)
            }
            SlashCommand::Agent { subcommand } => {
                // For the TUI path, /agent traits is a simple listing.
                // /agent compose runs a full model turn via rebuild-run-restore.
                match subcommand {
                    AgentSubcommand::Traits => {
                        let catalogue = bundled_catalogue();
                        (format_traits_listing(catalogue), false)
                    }
                    AgentSubcommand::Compose { traits, task } => {
                        if traits.is_empty() {
                            return Ok(("No traits provided. Usage: /agent compose security,skeptical,first-principles \"audit auth.rs\"".to_string(), false));
                        }
                        if task.trim().is_empty() {
                            return Ok((format!("No task provided. Usage: /agent compose {} \"<task description>\"", traits.join(",")), false));
                        }
                        let catalogue = bundled_catalogue();
                        let trait_refs: Vec<&str> = traits.iter().map(String::as_str).collect();
                        match compose_agent(catalogue, &trait_refs, &task) {
                            Err(ComposeError::EmptyTraits) => ("No traits provided. Usage: /agent compose security,skeptical,first-principles \"audit auth.rs\"".to_string(), false),
                            Err(ComposeError::UnknownTrait(name)) => (format!("Unknown trait: {name}. Run /agent traits to list available traits."), false),
                            Err(ComposeError::ConflictingTraits { dim, a, b }) => (
                                format!(
                                    "Conflicting traits in dimension \"{dim}\": \"{a}\" and \"{b}\" cannot be combined.\nRemove one, or use traits from different dimensions.\n(A future version will add --allow-conflicts.)"
                                ),
                                false,
                            ),
                            Err(ComposeError::ParseError(msg)) => (format!("Trait catalogue parse error: {msg}"), false),
                            Ok(composed) => {
                                let header = format!("Composing agent with traits: {}", composed.traits.join(", "));
                                let original_system_prompt = self.system_prompt.clone();
                                let mut composed_system_prompt = vec![composed.prompt.clone()];
                                composed_system_prompt.extend(original_system_prompt.iter().cloned());

                                let rebuild = build_runtime_with_tui_slot(
                                    self.runtime.session().clone(),
                                    self.model.clone(),
                                    composed_system_prompt,
                                    true,
                                    true,
                                    self.allowed_tools.clone(),
                                    self.permission_mode,
                                    None,
                                    self.tui_slot.clone(),
                                    self.agent_manager.clone(),
                                );
                                match rebuild {
                                    Err(e) => (format!("agent compose: failed to build runtime: {e}"), false),
                                    Ok(new_runtime) => {
                                        self.runtime = new_runtime;
                                        let turn_result = self.run_turn(&task);
                                        let restore = build_runtime_with_tui_slot(
                                            self.runtime.session().clone(),
                                            self.model.clone(),
                                            original_system_prompt.clone(),
                                            true,
                                            true,
                                            self.allowed_tools.clone(),
                                            self.permission_mode,
                                            None,
                                            self.tui_slot.clone(),
                                            self.agent_manager.clone(),
                                        );
                                        self.system_prompt = original_system_prompt;
                                        if let Ok(restored) = restore {
                                            self.runtime = restored;
                                        }
                                        if let Err(e) = turn_result {
                                            (format!("{header}\nagent compose turn failed: {e}"), false)
                                        } else {
                                            (header, false)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            SlashCommand::Skill { subcommand } => {
                match subcommand {
                    SkillSubcommand::Load { name } => {
                        let cwd = env::current_dir().unwrap_or_default();
                        match load_skill_body(&name, &cwd) {
                            Ok(body) => {
                                // Prepend skill body to system prompt and rebuild runtime
                                // so it takes effect on the very next turn.
                                let marker = format!("# skill:{name} —");
                                self.system_prompt.retain(|s| !s.contains(&marker));
                                self.system_prompt.insert(0, body);
                                let session = self.runtime.session().clone();
                                match build_runtime_with_tui_slot(
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
                                ) {
                                    Ok(rt) => {
                                        self.runtime = rt;
                                        runtime::otel::skill_activated(&name, "user");
                                        (format!("Skill '{name}' loaded — active for the next turn."), false)
                                    }
                                    Err(e) => (format!("skill load: runtime rebuild failed: {e}"), false),
                                }
                            }
                            Err(msg) => (msg, false),
                        }
                    }
                    SkillSubcommand::List => {
                        let cwd = env::current_dir().unwrap_or_default();
                        let output = handle_skills_slash_command(Some("list"), &cwd);
                        (output.unwrap_or_else(|e| format!("skill list: {e}")), false)
                    }
                    SkillSubcommand::Suggest { prompt } => {
                        let cwd = env::current_dir().unwrap_or_default();
                        use commands::{format_suggestions, match_triggers as mt};
                        use commands::agents::{discover_skill_roots, load_skills_from_roots};
                        use runtime::{ContentBlock, MessageRole};
                        let prompt_text = prompt.unwrap_or_default();
                        if prompt_text.is_empty() {
                            // Fall back to the last user message in session history.
                            let last_user = self.runtime.session().messages.iter().rev()
                                .filter(|m| m.role == MessageRole::User)
                                .find_map(|m| m.blocks.iter().find_map(|b| {
                                    if let ContentBlock::Text { text } = b { Some(text.clone()) } else { None }
                                }))
                                .unwrap_or_default();
                            if last_user.is_empty() {
                                ("No prompt provided. Usage: /skill suggest <prompt>".to_string(), false)
                            } else {
                                let roots = discover_skill_roots(&cwd);
                                let skills = load_skills_from_roots(&roots).unwrap_or_default();
                                let skill_refs: Vec<&commands::agents::SkillSummary> = skills.iter().collect();
                                let matches = mt(&last_user, &skill_refs);
                                (format_suggestions(&matches, &last_user), false)
                            }
                        } else {
                            let roots = discover_skill_roots(&cwd);
                            let skills = load_skills_from_roots(&roots).unwrap_or_default();
                            let skill_refs: Vec<&commands::agents::SkillSummary> = skills.iter().collect();
                            let matches = mt(&prompt_text, &skill_refs);
                            (format_suggestions(&matches, &prompt_text), false)
                        }
                    }
                    SkillSubcommand::Chains => {
                        let cwd = env::current_dir().unwrap_or_default();
                        use commands::agents::{discover_skill_roots, load_skills_from_roots};
                        use commands::render_chains_graph;
                        let roots = discover_skill_roots(&cwd);
                        let skills = load_skills_from_roots(&roots).unwrap_or_default();
                        let all_skills: std::collections::HashMap<String, commands::agents::SkillSummary> =
                            skills.into_iter().map(|s| (s.name.to_ascii_lowercase(), s)).collect();
                        (render_chains_graph(&all_skills), false)
                    }
                }
            }
            SlashCommand::Goal { action } => {
                (self.run_goal_command(action.as_deref()), false)
            }
            SlashCommand::FileCache { .. } => {
                ("Use /file-cache list, stats, or forget <path>".to_string(), false)
            }
            SlashCommand::CmdCache { .. } => {
                ("Use /cmd-cache list, stats, prune, or forget <cmd>".to_string(), false)
            }
            SlashCommand::Profile { action } => {
                let msg = render_profile_command(action.as_deref());
                (msg, false)
            }
            SlashCommand::Unknown(name) => {
                // Intercepted in handle_repl_command_tui. This arm is unreachable.
                (format!("Unknown slash command: /{name}"), false)
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

    /// `/output-style [precise|condensed]` — get or set the response style.
    ///
    /// When `style` is `None`, prints current setting.
    /// When `style` is `Some("precise")` or `Some("condensed")`, applies the
    /// change, persists it to `~/.anvil/config.json`, and rebuilds the runtime
    /// so the new system prompt takes effect immediately.
    fn set_output_style(&mut self, style: Option<String>) -> Result<String, Box<dyn std::error::Error>> {
        const TERSE_SKILL_BODY: &str = include_str!("../../commands/bundled/skills/terse/SKILL.md");
        const TERSE_MARKER: &str = "# terse — token-economical response style";
        const CUSTOM_STYLE_MARKER: &str = "# __anvil_custom_output_style__";

        let Some(style_str) = style else {
            return Ok(format!("Output style: {}", self.output_style.as_str()));
        };

        // ── Control tokens ────────────────────────────────────────────────────
        if style_str.eq_ignore_ascii_case("list") {
            let styles_dir = runtime::default_output_styles_dir();
            let mut registry = runtime::OutputStyleRegistry::new();
            registry.ensure_loaded(&styles_dir);
            return Ok(registry.list_display());
        }

        if style_str.eq_ignore_ascii_case("reset") {
            return self.set_output_style(Some("precise".to_string()));
        }

        // ── Resolve through registry (user wins on name collision) ────────────
        let styles_dir = runtime::default_output_styles_dir();
        let mut registry = runtime::OutputStyleRegistry::new();
        registry.ensure_loaded(&styles_dir);

        let new_style = match registry.resolve(&style_str) {
            Some(s) => s,
            None => {
                return Err(format!(
                    "Unknown output style '{style_str}'. Run `/output-style list` to see available styles."
                )
                .into());
            }
        };

        if new_style == self.output_style {
            return Ok(format!("Output style already set to: {}", self.output_style.as_str()));
        }

        // ── Update system_prompt ──────────────────────────────────────────────
        // Remove any existing terse block and custom style block.
        self.system_prompt.retain(|s| !s.contains(TERSE_MARKER) && !s.contains(CUSTOM_STYLE_MARKER));

        let is_condensed = matches!(
            new_style,
            OutputStyle::BuiltIn(runtime::BuiltInStyle::Condensed)
        );
        if is_condensed {
            self.system_prompt.insert(0, TERSE_SKILL_BODY.to_string());
        } else if let Some(fragment) = new_style.prompt_fragment() {
            // Custom style: prepend the user-defined fragment with a marker so
            // we can reliably strip it when switching styles later.
            let body = format!("{CUSTOM_STYLE_MARKER}\n{fragment}");
            self.system_prompt.insert(0, body);
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

        let style_name = new_style.as_str().to_string();
        save_output_style(new_style.clone());
        self.output_style = new_style;

        let status = if is_condensed {
            "terse rules active".to_string()
        } else if self.output_style.prompt_fragment().is_some() {
            format!("custom style '{style_name}' active")
        } else {
            "default voice".to_string()
        };

        Ok(format!("Output style: {style_name} — {status} for next turn."))
    }

    /// `/effort [level]` — display or change the per-session effort level.
    ///
    /// With no argument, prints the current level.  With a level name
    /// (`low | medium | high | xhigh`), updates the session override and
    /// applies it to the environment immediately so the next turn picks it up.
    fn set_effort(&mut self, level: Option<String>) -> String {
        let Some(level_str) = level else {
            return format!("Effort: {} (low | medium | high | xhigh)", self.effort_level.as_str());
        };
        match EffortLevel::from_str(&level_str) {
            Some(new_level) => {
                self.effort_level = new_level;
                new_level.apply_to_env();
                format!("Effort set to: {}", new_level.as_str())
            }
            None => format!(
                "Unknown effort level '{level_str}'. Use: low, medium, high, xhigh."
            ),
        }
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
    /// T4-M: release-pipeline pre-flight self-check.
    ///
    /// Runs the same gates `scripts/release.sh` enforces, before invoking the
    /// actual build. Surfaces problems as ✓/✗ rows so the user can fix them
    /// without burning the 5-min cross-compile cycle just to discover a
    /// missing RELEASE-NOTES file or a dirty tree.
    ///
    /// Checks (each independent — one failure does not skip the rest):
    ///   1. Cargo.toml version reads + matches a `vX.Y.Z` shape
    ///   2. RELEASE-NOTES-vX.Y.Z.md exists and is non-trivial (>10 lines)
    ///   3. Git working tree is clean (no uncommitted/staged changes)
    ///   4. HEAD matches the local tag for this version (if the tag exists)
    ///   5. The local tag matches the remote tag (if both exist)
    ///   6. /opt/homebrew/bin/anvil is a brew symlink, not a shadowed file
    ///   7. `gh auth status` succeeds (release upload needs it)
    fn run_release_doctor() -> String {
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

    /// T4-O: candidate ANVIL.md / MEMORY.md / instructions paths for
    /// hot-reload tracking. Walks from cwd up to root for ANVIL.md and adds
    /// the user's MEMORY.md from `~/.anvil/`. Order doesn't matter — we just
    /// need every file the system prompt depends on.
    fn instructions_candidate_paths() -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        if let Ok(cwd) = env::current_dir() {
            let mut cursor: Option<&Path> = Some(cwd.as_path());
            while let Some(dir) = cursor {
                for name in &[
                    "ANVIL.md",
                    "ANVIL.local.md",
                    ".anvil/ANVIL.md",
                    ".anvil/instructions.md",
                ] {
                    out.push(dir.join(name));
                }
                cursor = dir.parent();
            }
        }
        if let Some(home) = dirs_next_home() {
            out.push(home.join(".anvil").join("MEMORY.md"));
            out.push(home.join(".anvil").join("ANVIL.md"));
        }
        out
    }

    /// T4-O: scan candidate instruction files; if any one's mtime differs
    /// from the cached value, rebuild `self.system_prompt` from scratch and
    /// return true. First-call seed is silent (no rebuild on the first turn
    /// after startup — the prompt was already built from these files in
    /// `LiveCli::new`).
    fn maybe_reload_instructions(&mut self) -> bool {
        let candidates = Self::instructions_candidate_paths();
        let mut current: std::collections::HashMap<PathBuf, std::time::SystemTime> =
            std::collections::HashMap::new();
        for path in &candidates {
            if let Ok(meta) = fs::metadata(path)
                && let Ok(mtime) = meta.modified() {
                    current.insert(path.clone(), mtime);
                }
        }

        // First call: just seed, don't rebuild.
        if self.instructions_mtime.is_empty() {
            self.instructions_mtime = current;
            return false;
        }

        // Compare. If any tracked file's mtime changed, OR any new file
        // appeared, OR any file disappeared — we need a rebuild.
        let changed = current.len() != self.instructions_mtime.len()
            || current.iter().any(|(p, t)| {
                self.instructions_mtime.get(p).map_or(true, |old| old != t)
            });

        if !changed {
            return false;
        }

        let provider = friendly_provider_label(&self.model);
        match build_system_prompt_with_identity(Some(self.model.clone()), provider, None) {
            Ok(new_prompt) => {
                self.system_prompt = new_prompt;
                self.instructions_mtime = current;
                eprintln!("⟳ ANVIL.md / MEMORY.md changed — system prompt reloaded");
                true
            }
            Err(_) => {
                // Don't poison the session if rebuild fails — keep the old
                // prompt and try again next turn.
                false
            }
        }
    }

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

    /// `/login [provider]` or `/provider login` — refresh OAuth token from within REPL.
    /// Temporarily leaves the TUI to run the OAuth browser flow, then returns.
    fn run_inline_login(&self, provider: Option<&str>) -> String {
        let provider_name = provider.unwrap_or_else(|| {
            match detect_provider_kind(&self.model) {
                ProviderKind::AnvilApi => "anthropic",
                ProviderKind::OpenAi => "openai",
                ProviderKind::Gemini => "gemini",
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
            SlashCommand::Help { command } => {
                let text = if let Some(cmd) = command.as_deref() {
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
            SlashCommand::Clear { confirm, all_tabs: _ } => self.clear_session(confirm)?,
            SlashCommand::Cost => {
                self.print_cost();
                false
            }
            SlashCommand::Resume { session_path } => self.resume_session(session_path)?,
            SlashCommand::Config { section } => {
                Self::print_config(section.as_deref())?;
                false
            }
            SlashCommand::Memory { .. } => {
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
            SlashCommand::Export { format, path } => {
                self.export_session(format.as_deref(), path.as_deref())?;
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
            SlashCommand::Skill { subcommand } => {
                let cwd = env::current_dir().unwrap_or_default();
                match subcommand {
                    SkillSubcommand::Load { name } => {
                        match load_skill_body(&name, &cwd) {
                            Ok(body) => {
                                let marker = format!("# skill:{name} —");
                                self.system_prompt.retain(|s| !s.contains(&marker));
                                self.system_prompt.insert(0, body);
                                let session = self.runtime.session().clone();
                                match build_runtime_with_tui_slot(
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
                                ) {
                                    Ok(rt) => {
                                        self.runtime = rt;
                                        println!("Skill '{name}' loaded — active for the next turn.");
                                    }
                                    Err(e) => eprintln!("skill load: runtime rebuild failed: {e}"),
                                }
                            }
                            Err(msg) => eprintln!("{msg}"),
                        }
                    }
                    SkillSubcommand::List => {
                        match handle_skills_slash_command(Some("list"), &cwd) {
                            Ok(output) => println!("{output}"),
                            Err(e) => eprintln!("skill list: {e}"),
                        }
                    }
                    SkillSubcommand::Suggest { prompt } => {
                        use commands::{format_suggestions, match_triggers as mt};
                        use commands::agents::{discover_skill_roots, load_skills_from_roots};
                        use runtime::{ContentBlock, MessageRole};
                        let prompt_text = prompt.unwrap_or_default();
                        let effective = if prompt_text.is_empty() {
                            self.runtime.session().messages.iter().rev()
                                .filter(|m| m.role == MessageRole::User)
                                .find_map(|m| m.blocks.iter().find_map(|b| {
                                    if let ContentBlock::Text { text } = b { Some(text.clone()) } else { None }
                                }))
                                .unwrap_or_default()
                        } else {
                            prompt_text
                        };
                        if effective.is_empty() {
                            println!("No prompt provided. Usage: /skill suggest <prompt>");
                        } else {
                            let roots = discover_skill_roots(&cwd);
                            let skills = load_skills_from_roots(&roots).unwrap_or_default();
                            let skill_refs: Vec<&commands::agents::SkillSummary> = skills.iter().collect();
                            let matches = mt(&effective, &skill_refs);
                            println!("{}", format_suggestions(&matches, &effective));
                        }
                    }
                    SkillSubcommand::Chains => {
                        use commands::agents::{discover_skill_roots, load_skills_from_roots};
                        use commands::render_chains_graph;
                        let roots = discover_skill_roots(&cwd);
                        let skills = load_skills_from_roots(&roots).unwrap_or_default();
                        let all_skills: std::collections::HashMap<String, commands::agents::SkillSummary> =
                            skills.into_iter().map(|s| (s.name.to_ascii_lowercase(), s)).collect();
                        println!("{}", render_chains_graph(&all_skills));
                    }
                }
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
            SlashCommand::Mcp { action } => {
                println!("{}", self.run_mcp_command(action.as_deref()));
                false
            }
            SlashCommand::Productivity => {
                println!("{}", self.run_productivity_command());
                false
            }
            SlashCommand::Knowledge { action } => {
                println!("{}", self.run_knowledge_command(action.as_deref()));
                false
            }
            SlashCommand::Daily { date } => {
                println!("{}", self.run_daily_command(date.as_deref()));
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
            SlashCommand::Doctor { mode } => {
                let out = match mode.as_deref() {
                    Some("release") => Self::run_release_doctor(),
                    _ => self.run_doctor(),
                };
                println!("{}", out);
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
            // T5-Ssh: embedded SSH client (REPL fallback — full UI requires TUI)
            SlashCommand::Ssh { args: _ } => {
                println!("/ssh: embedded SSH client requires TUI mode (run `anvil` interactively).");
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
            SlashCommand::OutputStyle { style } => {
                match self.set_output_style(style) {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => eprintln!("output-style error: {e}"),
                }
                false
            }
            SlashCommand::Effort { level } => {
                println!("{}", self.set_effort(level));
                false
            }
            SlashCommand::ReviewPr { number } => {
                println!("{}", self.run_review_pr_command(number.as_deref()));
                false
            }
            SlashCommand::RemoteControl { action } => {
                println!("{}", self.run_remote_control_command(action.as_deref()));
                false
            }
            SlashCommand::Loop { prompt } => {
                println!("Loop mode: {}", prompt.as_deref().unwrap_or("no prompt set"));
                false
            }
            SlashCommand::Focus => {
                println!("Focus view toggle (TUI only)");
                false
            }
            // v2.2.6 ghost commands — Phase 1 real implementations
            SlashCommand::Tab { action } => {
                println!(
                    "Tab management is only available in TUI mode. Action: {}",
                    action.as_deref().unwrap_or("list")
                );
                false
            }
            SlashCommand::Fork => {
                println!("Conversation branching (/fork) is only available in TUI mode.");
                false
            }
            SlashCommand::Share { action } => {
                if !runtime::vault_is_session_unlocked() {
                    println!("This command requires the vault to be unlocked. Run /vault unlock first.");
                    return Ok(false);
                }
                let output = self.run_share_command_repl(action.as_deref());
                println!("{output}");
                false
            }
            SlashCommand::Audit => {
                println!("{}", self.run_security_command(Some("scan")));
                println!("{}", Self::run_deps_command(Some("audit")));
                println!("{}", self.run_vault_command(Some("verify")));
                false
            }
            SlashCommand::Restart { soft } => {
                if soft {
                    // Soft restart: reload config in-place.
                    Self::print_config(None)?;
                    println!("Config reloaded.");
                } else {
                    // Full restart: prompt then respawn.
                    print!("Save and restart Anvil? [y/N] ");
                    let _ = io::stdout().flush();
                    let mut choice = String::new();
                    let _ = io::stdin().read_line(&mut choice);
                    let answer = choice.trim().to_ascii_lowercase();
                    if answer == "y" || answer == "yes" {
                        let ctx = get_respawn_ctx();
                        let session_id = self.session.id.clone();
                        match respawn::respawn(&ctx, "user /restart", &session_id) {
                            Ok(respawn::RespawnOutcome::Respawned) => {
                                // Unreachable — exec replaced us.
                            }
                            Ok(respawn::RespawnOutcome::PromptUser(msg)) => {
                                println!("{msg}");
                                std::process::exit(42);
                            }
                            Err(e) => {
                                eprintln!("Restart failed: {e}");
                            }
                        }
                    } else {
                        println!("Restart cancelled.");
                    }
                }
                false
            }
            SlashCommand::Agent { subcommand } => {
                if let Err(e) = self.run_agent_command(subcommand) {
                    eprintln!("agent: {e}");
                }
                false
            }
            SlashCommand::Goal { action } => {
                println!("{}", self.run_goal_command(action.as_deref()));
                false
            }
            SlashCommand::FileCache { .. } => {
                println!("Use /file-cache list, stats, or forget <path>");
                false
            }
            SlashCommand::CmdCache { .. } => {
                println!("Use /cmd-cache list, stats, prune, or forget <cmd>");
                false
            }
            SlashCommand::Profile { action } => {
                println!("{}", render_profile_command(action.as_deref()));
                false
            }
            SlashCommand::Unknown(name) => {
                eprintln!("{}", render_unknown_repl_command(&name));
                false
            }
        })
    }

    // ── /goal command ─────────────────────────────────────────────────────────

    /// Handle `/goal [new "<desc>"|list|resume <id>|pause [<id>]|done [<id>]|show [<id>]]`.
    fn run_goal_command(&mut self, args: Option<&str>) -> String {
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
                match mgr.new_goal(desc) {
                    Ok(goal) => format!(
                        "Goal created: {}\nStatus: {}\n{}",
                        goal.id, goal.status, goal.description
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

    // ── /agent command ────────────────────────────────────────────────────────

    /// Handle `/agent compose <traits> "<task>"` and `/agent traits`.
    ///
    /// For `compose`: loads the bundled catalogue, calls `compose_agent`, then
    /// runs the task as a subagent turn.  The turn runs through the existing
    /// runtime with the composed system prompt prepended — using the same path
    /// as `/fast` mode (rebuild runtime with modified prompt, run one turn,
    /// restore).  This keeps it on the `build_runtime_with_tui_slot` path and
    /// inside the existing `AgentManager`.
    ///
    /// For `traits`: prints `format_traits_listing` to stdout.
    fn run_agent_command(
        &mut self,
        subcommand: AgentSubcommand,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match subcommand {
            AgentSubcommand::Traits => {
                let catalogue = bundled_catalogue();
                println!("{}", format_traits_listing(catalogue));
                Ok(())
            }
            AgentSubcommand::Compose { traits, task } => {
                if traits.is_empty() {
                    println!(
                        "No traits provided. Usage: /agent compose security,skeptical,first-principles \"audit auth.rs\""
                    );
                    return Ok(());
                }
                if task.trim().is_empty() {
                    println!(
                        "No task provided. Usage: /agent compose {} \"<task description>\"",
                        traits.join(",")
                    );
                    return Ok(());
                }

                let catalogue = bundled_catalogue();
                let trait_refs: Vec<&str> = traits.iter().map(String::as_str).collect();

                match compose_agent(catalogue, &trait_refs, &task) {
                    Err(ComposeError::EmptyTraits) => {
                        println!(
                            "No traits provided. Usage: /agent compose security,skeptical,first-principles \"audit auth.rs\""
                        );
                        Ok(())
                    }
                    Err(ComposeError::UnknownTrait(name)) => {
                        println!(
                            "Unknown trait: {name}. Run /agent traits to list available traits."
                        );
                        Ok(())
                    }
                    Err(ComposeError::ConflictingTraits { dim, a, b }) => {
                        println!(
                            "Conflicting traits in dimension \"{dim}\": \"{a}\" and \"{b}\" \
                             cannot be combined — they would fight over the same identity axis.\n\
                             Remove one of them, or use traits from different dimensions.\n\
                             (A future version will add --allow-conflicts for when you really want this.)"
                        );
                        Ok(())
                    }
                    Err(ComposeError::ParseError(msg)) => {
                        println!("Trait catalogue parse error: {msg}");
                        Ok(())
                    }
                    Ok(composed) => {
                        println!(
                            "Composing agent with traits: {}\n",
                            composed.traits.join(", ")
                        );

                        // Rebuild runtime with composed system prompt prepended,
                        // run one turn, then restore — same pattern as /fast mode.
                        let original_system_prompt = self.system_prompt.clone();
                        let mut composed_system_prompt = vec![composed.prompt.clone()];
                        composed_system_prompt.extend(original_system_prompt.iter().cloned());

                        self.runtime = build_runtime_with_tui_slot(
                            self.runtime.session().clone(),
                            self.model.clone(),
                            composed_system_prompt,
                            true,
                            true,
                            self.allowed_tools.clone(),
                            self.permission_mode,
                            None,
                            self.tui_slot.clone(),
                            self.agent_manager.clone(),
                        )?;

                        let turn_result = self.run_turn(&task);

                        // Restore original system prompt regardless of outcome.
                        let restore_result = build_runtime_with_tui_slot(
                            self.runtime.session().clone(),
                            self.model.clone(),
                            original_system_prompt.clone(),
                            true,
                            true,
                            self.allowed_tools.clone(),
                            self.permission_mode,
                            None,
                            self.tui_slot.clone(),
                            self.agent_manager.clone(),
                        );
                        self.system_prompt = original_system_prompt;
                        if let Ok(restored) = restore_result {
                            self.runtime = restored;
                        }

                        turn_result
                    }
                }
            }
        }
    }

    fn persist_session(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.runtime.session().save_to_path(&self.session.path)?;
        Ok(())
    }

    /// Record this session in the daily summary store and print any open items.
    ///
    /// Called once at normal session exit (from both TUI and non-TUI paths).
    /// Failures are swallowed so a write error never prevents Anvil from exiting.
    fn record_daily(&self) {
        use runtime::{DailyStore, SessionSummary, extract_tasks};

        let session_data = self.runtime.session();
        let messages = &session_data.messages;

        // Extract tasks from the conversation history.
        let (tasks_completed, tasks_open) = extract_tasks(messages);

        // Collect modified files from ToolResult outputs that mention a path.
        let files_modified = collect_modified_files(messages);

        // Count nominations generated this session.
        let nominations_generated = {
            let store = runtime::nominations::NominationStore::new();
            store.list(Some(runtime::nominations::NominationStatus::Pending)).len()
        };

        let tokens_used = self.runtime.usage().cumulative_usage().total_tokens();
        let duration_ms = self.session_start.elapsed().as_millis() as u64;
        let duration_secs = duration_ms / 1000;
        let messages_count = messages.len();

        // Count tool-use blocks across all messages for OTel.
        let tool_count = messages.iter().flat_map(|m| &m.blocks).filter(|b| {
            matches!(b, runtime::ContentBlock::ToolUse { .. })
        }).count() as u64;

        // Emit session_end OTel event (no-op when disabled).
        {
            let cost = self.runtime.usage().cumulative_usage();
            let cost_str = format!("{:.6}", cost.total_tokens() as f64 * 0.000_003);
            runtime::otel::session_end(
                &self.session.id,
                duration_ms,
                u64::from(tokens_used),
                &cost_str,
                tool_count,
            );
        }

        let summary = SessionSummary {
            session_id: self.session.id.clone(),
            model: self.model.clone(),
            duration_secs,
            tokens_used,
            messages_count,
            tasks_completed,
            tasks_open,
            files_modified,
            nominations_generated,
            credentials_auto_vaulted: 0,
        };

        let store = DailyStore::new();
        if let Err(e) = store.record_session(summary) {
            // Non-fatal: daily summaries are best-effort.
            eprintln!("[daily] failed to record session: {e}");
            return;
        }

        // Print open items as a reminder.
        let updated = store.today();
        let open = store.reconcile(&updated);
        if !open.is_empty() {
            eprintln!();
            eprintln!("Open items from today ({}):", updated.date);
            for (i, item) in open.iter().enumerate() {
                eprintln!("  {}. {item}", i + 1);
            }
        }
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

    /// Return SkillSummary records for skills currently loaded in the system prompt.
    /// Looks for `# skill:<name> —` markers inserted by `/skill load`.
    fn loaded_skills_snapshot(&self) -> Vec<commands::agents::SkillSummary> {
        use commands::agents::{discover_skill_roots, load_skills_from_roots};
        let cwd = std::env::current_dir().unwrap_or_default();
        let roots = discover_skill_roots(&cwd);
        let all_skills = load_skills_from_roots(&roots).unwrap_or_default();
        let mut loaded = Vec::new();
        for prompt_part in &self.system_prompt {
            for skill in &all_skills {
                let marker = format!("# skill:{} —", skill.name);
                if prompt_part.contains(&marker) {
                    loaded.push(skill.clone());
                    break;
                }
            }
        }
        loaded
    }

    /// Emit a post-turn skill hint when the user prompt matches trigger keywords.
    /// This is the "turn-end footer" approach — informational, never blocks the turn.
    fn maybe_emit_skill_hint(&self, last_user_input: &str) {
        use commands::agents::{discover_skill_roots, load_skills_from_roots};
        let cwd = std::env::current_dir().unwrap_or_default();
        let roots = discover_skill_roots(&cwd);
        let skills: Vec<commands::agents::SkillSummary> = match load_skills_from_roots(&roots) {
            Ok(s) => s,
            Err(_) => return,
        };
        let skills_with_triggers: Vec<&commands::agents::SkillSummary> =
            skills.iter().filter(|s| !s.triggers.is_empty()).collect();
        if skills_with_triggers.is_empty() {
            return;
        }
        let matches = match_triggers(last_user_input, &skills_with_triggers);
        if let Some(hint) = format_suggestions_hint(&matches) {
            println!("{hint}");
        }
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
        format: Option<&str>,
        requested_path: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let export_path = resolve_export_path(requested_path, self.runtime.session())?;
        let content = if format == Some("md") {
            render_export_markdown(self.runtime.session())
        } else {
            render_export_text(self.runtime.session())
        };
        fs::write(&export_path, content)?;
        let fmt_label = if format == Some("md") { "markdown" } else { "text" };
        println!(
            "Export\n  Result           wrote {fmt_label} transcript\n  File             {}\n  Messages         {}",
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
            Some("rename") => {
                // T3-J: set or clear the active session's friendly name.
                // Empty `target` clears it; otherwise the name is validated
                // (1..=64 chars of [A-Za-z0-9_-]) and uniqueness-checked
                // by session_meta::set_session_name.
                let new_name = target.unwrap_or("").trim();
                match session_meta::set_session_name(&self.session.id, new_name) {
                    Ok(()) => {
                        if new_name.is_empty() {
                            println!("Session name cleared (id: {})", self.session.id);
                        } else {
                            println!(
                                "Session renamed\n  id    {}\n  name  {}\n  Tip   resume later with: anvil --resume {}",
                                self.session.id, new_name, new_name,
                            );
                        }
                        Ok(false)
                    }
                    Err(e) => {
                        println!("Rename failed: {e}");
                        Ok(false)
                    }
                }
            }
            Some(other) => {
                println!(
                    "Unknown /session action '{other}'. Use /session list, /session switch <id>, or /session rename <name>.",
                );
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

    /// TUI-safe variant: returns `(output, session_changed)` instead of printing.
    fn handle_plugins_command_tui(
        &mut self,
        action: Option<&str>,
        target: Option<&str>,
    ) -> Result<(String, bool), Box<dyn std::error::Error>> {
        let cwd = env::current_dir()?;
        let loader = ConfigLoader::default_for(&cwd);
        let runtime_config = loader.load()?;
        let mut manager = build_plugin_manager(&cwd, &loader, &runtime_config);
        let result = handle_plugins_slash_command(action, target, &mut manager)?;
        if result.reload_runtime {
            self.reload_runtime_features()?;
        }
        Ok((result.message, false))
    }

    /// TUI-safe variant of `handle_session_command`: returns `(output, session_changed)`.
    fn handle_session_command_tui(
        &mut self,
        action: Option<&str>,
        target: Option<&str>,
    ) -> Result<(String, bool), Box<dyn std::error::Error>> {
        match action {
            None | Some("list") => {
                let list = render_session_list(&self.session.id)?;
                Ok((list, false))
            }
            Some("switch") => {
                let Some(target) = target else {
                    return Ok(("Usage: /session switch <session-id>".to_string(), false));
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
                Ok((format!(
                    "Session switched\n  Active session   {}\n  File             {}\n  Messages         {}",
                    self.session.id,
                    self.session.path.display(),
                    message_count,
                ), true))
            }
            Some("rename") => {
                // T3-J TUI variant. See handle_session_command for behavior.
                let new_name = target.unwrap_or("").trim();
                match session_meta::set_session_name(&self.session.id, new_name) {
                    Ok(()) => {
                        let msg = if new_name.is_empty() {
                            format!("Session name cleared (id: {})", self.session.id)
                        } else {
                            format!(
                                "Session renamed\n  id    {}\n  name  {}\n  Tip   resume later with: anvil --resume {}",
                                self.session.id, new_name, new_name,
                            )
                        };
                        Ok((msg, false))
                    }
                    Err(e) => Ok((format!("Rename failed: {e}"), false)),
                }
            }
            Some(other) => {
                Ok((format!(
                    "Unknown /session action '{other}'. Use /session list, /session switch <id>, or /session rename <name>.",
                ), false))
            }
        }
    }

    /// TUI-safe variant of `resume_session`: returns `(output, session_changed)`.
    fn resume_session_tui(
        &mut self,
        session_path: Option<String>,
    ) -> Result<(String, bool), Box<dyn std::error::Error>> {
        let Some(session_ref) = session_path else {
            return Ok(("Usage: /resume <session-path>".to_string(), false));
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
        Ok((format_resume_report(
            &self.session.path.display().to_string(),
            message_count,
            self.runtime.usage().turns(),
        ), true))
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

    /// `/share [stop|list]` handler for the CLI REPL (non-TUI) surface.
    ///
    /// The CLI REPL has no `Tab` struct, so we build share messages directly
    /// from the session's `ConversationMessage` list (user + assistant only).
    /// `list` and `stop` work the same as in TUI mode.
    ///
    /// The caller MUST check `runtime::vault_is_session_unlocked()` before
    /// calling this method.
    fn run_share_command_repl(&mut self, action: Option<&str>) -> String {
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
                    scrollback_state: crate::tui::scrollback::ScrollbackState::live(),
                };
                self.share_manager.stop_share(&synthetic)
            }
            "list" => self.share_manager.list_shares(),
            // No subcommand → share the current REPL session.
            _ => {
                // Build share messages from the session (user + assistant only).
                let messages: Vec<runtime::ShareMessage> = self
                    .runtime
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
