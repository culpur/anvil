// Edition 2024: env::set_var/remove_var require unsafe
#![allow(unsafe_code)]

mod agents;
mod auth;
mod cmd_ai;
mod cmd_cache;
mod cmd_provider;
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
mod ollama_bench;
mod ollama_cmds;
mod ollama_manage;
mod ollama_requantize;
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
    detect_provider_kind, max_tokens_for_model, provider_display_name, slug_to_provider_kind,
    ToolDefinition,
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
    check_plugin_install_policy, load_requirements,
    load_system_prompt, render_history_context, render_qmd_context,
    ArchiveEntry, CompactionConfig, CompletedTaskInfo,
    ConfigLoader, ConversationRuntime, CronDaemon,
    EffortLevel, HistoryArchiver, NotificationKind, NotificationPayload, OutputStyle,
    PermissionMode, PolicyCheckError, QmdClient, Session, TaskManager, TokenUsage, UsageTracker,
};
use crossterm::terminal;
use serde_json::json;
use tools::GlobalToolRegistry;
use tui::{AnvilTui, ConfigureAction, InFlightInterruption, ReadResult, TuiEvent, TuiSender};

use auth::{
    run_login, run_logout,
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
    build_plugin_manager, build_runtime, build_runtime_for_provider, build_runtime_with_tui_slot,
    resolve_cli_auth_source, CliPermissionPrompter, DefaultRuntimeClient, CliToolExecutor,
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

/// RAII guard that removes this process's cross-session agent snapshot file
/// on a clean drop (CC-139-F1, #462).  The panic hook handles the dirty exit
/// case so a crashed process never leaves a stale `~/.anvil/agents/<pid>.json`
/// behind.  Best-effort — errors are intentionally swallowed.
struct AgentSnapshotGuard;

impl Drop for AgentSnapshotGuard {
    fn drop(&mut self) {
        runtime::agent_snapshot::clear_snapshot(std::process::id());
    }
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

    // Cross-session agent snapshot cleanup on graceful exit (CC-139-F1, #462).
    // The snapshot itself is written by AgentManager throughout the session;
    // this guard ensures the file is reaped on a clean drop.  The panic hook
    // below also clears it so a crash doesn't leave a stale file.
    let _agent_snapshot_guard = AgentSnapshotGuard;

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
    // CC parity (v2.2.14): only leave alt-screen if we would have entered it.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        if crate::tui::alternate_screen_enabled() {
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::event::DisableMouseCapture,
                crossterm::terminal::LeaveAlternateScreen
            );
        } else {
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::event::DisableMouseCapture,
            );
        }
        // Reap the cross-session agent snapshot so a panic doesn't leave a
        // stale file behind (CC-139-F1, #462).
        runtime::agent_snapshot::clear_snapshot(std::process::id());
        default_hook(info);
    }));

    if let Err(error) = run() {
        // Ensure terminal is cleaned up on error exit too
        let _ = crossterm::terminal::disable_raw_mode();
        if crate::tui::alternate_screen_enabled() {
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::event::DisableMouseCapture,
                crossterm::terminal::LeaveAlternateScreen
            );
        } else {
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::event::DisableMouseCapture,
            );
        }
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

/// CC-139-F3: `/scroll-speed [N]` — get/set mouse-wheel speed.
///
/// `None` / empty input prints the current value. An integer 1..=10
/// sets it via the runtime's process-scoped AtomicU8, taking effect on
/// the next wheel event (no redraw needed). Out-of-range and
/// non-numeric input return a usage message without mutating state.
fn run_scroll_speed_command(arg: Option<&str>) -> String {
    let trimmed = arg.map(str::trim).filter(|s| !s.is_empty());
    match trimmed {
        None => format!(
            "Current scroll-speed: {} lines/tick. Use /scroll-speed N (1..=10) to change.",
            runtime::get_scroll_speed()
        ),
        Some(raw) => match raw.parse::<u8>() {
            Ok(n) if (1..=10).contains(&n) => {
                runtime::set_scroll_speed(n);
                format!("Scroll speed set to {n} lines per wheel tick.")
            }
            Ok(_) => "Scroll speed must be between 1 and 10.".to_string(),
            Err(_) => format!("Not a number: {raw}. Usage: /scroll-speed [1..=10]"),
        },
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
        SlashCommand::Memory { action } => Ok(ResumeCommandOutcome {
            session: session.clone(),
            // Phase 2 / Bucket 2: resume-replay has no live runtime, so the
            // working-memory view falls back to the static explanation. All
            // other tiers run identically.
            message: Some(commands::handle_memory_command(
                action.as_deref(),
                &commands::MemoryContext::default(),
            )),
        }),
        SlashCommand::Ollama { args } => {
            let ollama_host = std::env::var("OLLAMA_HOST")
                .unwrap_or_else(|_| "http://localhost:11434".to_string());
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(crate::ollama_cmds::run_ollama_command(args.as_deref(), &ollama_host)),
            })
        }
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
        | SlashCommand::ScrollSpeed { .. }
        | SlashCommand::Import { .. }
        | SlashCommand::Profile { .. }
        | SlashCommand::Cursor { .. }
        | SlashCommand::HubStatus { .. }
        | SlashCommand::Layout { .. }
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

    // Restore the model the session was last using. Falls back to
    // DEFAULT_MODEL only if the sidecar is missing (older sessions, or one
    // that never persisted). This prevents Ollama sessions from being
    // re-built on Anthropic and then erroring on missing credentials.
    let saved_model = session_meta::get_session_model(&most_recent.id)
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    // Build a fresh LiveCli then immediately swap in the loaded session.
    let mut cli = LiveCli::new(
        saved_model,
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

    // Keep a prototype sender so push_tab_runtime can stamp new per-tab senders.
    let sender_prototype = sender.clone();

    // Install the TUI sender so all model/tool output is routed to it.
    cli.enable_tui(sender);

    // Bootstrap tab (index 0) already has a runtime installed by LiveCli::new.
    tui.mark_tab_has_runtime(0);

    // v2.2.14 TUI-1: share the bootstrap tab's cancel flag with its runtime
    // so Ctrl+C in the TUI's in-flight handler cancels the streaming turn.
    if let Some(token) = tui.tab_cancel_token(0) {
        cli.active_runtime_mut().set_cancel_handle(token);
    }

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

    // v2.2.16: first-launch layout intro toast.
    // Show once when `tui_layout` is absent from config (user hasn't
    // configured it yet) AND `tui_layout_intro_seen` is not set.
    // After emitting, stamp `tui_layout_intro_seen: true` so it never repeats.
    {
        let config_path = anvil_home_dir().join("config.json");
        let (has_layout_key, intro_seen) = if let Ok(data) = fs::read_to_string(&config_path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&data) {
                let has_key = val.get("tui_layout").is_some();
                let seen = val
                    .get("tui_layout_intro_seen")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                (has_key, seen)
            } else {
                (false, false)
            }
        } else {
            (false, false)
        };
        if !has_layout_key && !intro_seen {
            tui.push_system(
                "New in v2.2.16: 6 TUI layouts (A–F). \
                 Try /layout list to see them, /layout <name> to switch. \
                 Your current layout (vertical-split-tabs) is the default."
                    .to_string(),
            );
            // Stamp intro seen so this never fires again.
            if let Ok(data) = fs::read_to_string(&config_path) {
                if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&data) {
                    if let Some(obj) = val.as_object_mut() {
                        obj.insert(
                            "tui_layout_intro_seen".to_string(),
                            serde_json::Value::Bool(true),
                        );
                        let _ = fs::write(&config_path, serde_json::to_string_pretty(&val).unwrap_or_default());
                    }
                }
            }
        }
    }

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
        let msgs = cli.active_runtime_mut().run_session_start_hooks();
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
                                        // Web viewer installs: gate is not applied here —
                                        // the AnvilHub web UI surfaces verification state
                                        // before the user clicks install.  REVOKED check
                                        // still runs inside install() unconditionally.
                                        let result = client.install(&p, &install_dir, false, true);
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
                                            let result = client.install(&p, &install_dir, false, true);
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
                    cli.active_tab_idx = tab_idx;
                    let tab_id = tui.tab_id_at(tab_idx).unwrap_or(tab_idx + 1);
                    let cancel_token = tui
                        .tab_cancel_token(tab_idx)
                        .expect("just-created tab must have a cancel token");
                    if let Err(e) = cli.push_tab_runtime(
                        tab_id,
                        &sender_prototype,
                        Session::new(),
                        cli.model.clone(),
                        cli.system_prompt.clone(),
                        true,
                        cli.allowed_tools.clone(),
                        cli.permission_mode,
                        cancel_token,
                    ) {
                        tui.push_system(format!("[Remote] Warning: per-tab runtime failed: {e}"));
                    } else {
                        tui.mark_tab_has_runtime(tab_idx);
                    }
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
                // Preprocessing (instruction hot-reload, pinned files, QMD context)
                // runs on the main thread; only the blocking API call is offloaded.
                if cli.maybe_reload_instructions() {
                    let sp = cli.system_prompt.clone();
                    cli.active_runtime_mut().replace_system_prompt(sp);
                }
                cli.inject_pinned_files_for_active_tab();
                cli.effort_level.apply_to_env();
                let effective_input = cli.build_input_with_qmd_context(&message);
                let active = cli.active_tab_idx;
                let active_tab_id = tui.tab_id_at(active).unwrap_or(active + 1);
                if let Err(reason) = cli.spawn_turn_for_tab(active, effective_input, cli.permission_mode) {
                    tui.push_system(format!("Cannot start turn: {reason}"));
                } else {
                    tui.set_tab_in_flight(active, true);
                    // Dispatch loop: handle user actions that arrive while the
                    // remote-triggered turn is in flight (tab switching, new
                    // tabs, etc.).  Only the originally-spawned tab's TurnDone
                    // (or ChannelClosed) ends this wait — v2.2.14 TUI-2 deep:
                    // do NOT rebind wait_tab_id on TabSwitched / new-tab / etc.
                    // The wait tracks the turn we just spawned, not whatever
                    // is on screen now. Background tabs' TurnDones are routed
                    // via `apply_tagged_event` and reaped after the wait.
                    let wait_tab_id = active_tab_id;
                    'remote_wait: loop {
                        match tui.wait_for_turn_end_for_tab(wait_tab_id)? {
                            InFlightInterruption::TurnDone
                            | InFlightInterruption::ChannelClosed => break 'remote_wait,
                            InFlightInterruption::TabSwitched => {
                                cli.active_tab_idx = tui.active_tab_index();
                                if !tui.is_any_tab_in_flight() { break 'remote_wait; }
                            }
                            InFlightInterruption::OpenNewTab => {
                                let new_session = create_managed_session_handle()?;
                                let tab_idx = tui.new_tab("new", cli.model.clone(), new_session.id.clone());
                                tui.switch_tab(tab_idx);
                                cli.active_tab_idx = tab_idx;
                                let tab_id = tui.tab_id_at(tab_idx).unwrap_or(tab_idx + 1);
                                let cancel_token = tui
                                    .tab_cancel_token(tab_idx)
                                    .expect("just-created tab must have a cancel token");
                                if let Err(e) = cli.push_tab_runtime(
                                    tab_id,
                                    &sender_prototype,
                                    Session::new(),
                                    cli.model.clone(),
                                    cli.system_prompt.clone(),
                                    true,
                                    cli.allowed_tools.clone(),
                                    cli.permission_mode,
                                    cancel_token,
                                ) {
                                    tui.push_system(format!("Warning: per-tab runtime failed: {e}"));
                                } else {
                                    tui.mark_tab_has_runtime(tab_idx);
                                }
                                tui.push_system(format!(
                                    "Opened tab {}  |  session {}",
                                    tab_idx + 1,
                                    new_session.id,
                                ));
                            }
                            InFlightInterruption::CloseActiveTab => {
                                let idx = tui.active_tab_index();
                                if idx != active {
                                    tui.close_tab_by_index(idx);
                                    cli.active_tab_idx = tui.active_tab_index();
                                }
                            }
                            InFlightInterruption::SlashCommand(line) => {
                                let trimmed = line.trim();
                                // /quit and /exit aren't recognised by SlashCommand::parse —
                                // the literal-match exit path lives in the main loop's
                                // ReadResult::Submit handler. Stash the command as a pending
                                // submission so the main loop picks it up after we break.
                                if matches!(trimmed, "/exit" | "/quit") {
                                    tui.set_pending_submission(trimmed.to_string());
                                    break 'remote_wait;
                                }
                                tui.push_system(format!("↳ executing held command: {line}"));
                                if let Some(command) = SlashCommand::parse(trimmed) {
                                    match cli.handle_repl_command_tui(command, &mut tui) {
                                        Ok(_) => {}
                                        Err(err) => tui.push_system(format!("Error: {err}")),
                                    }
                                    tui.set_thinking_enabled(cli.thinking_enabled);
                                    tui.set_effort_level(cli.effort_level.as_str());
                                }
                                // If the command opened a modal (e.g. /ssh, /configure),
                                // break out of the wait so the main read_input loop drives
                                // the modal. The background turn continues; we'll reap it
                                // on subsequent main-loop iterations.
                                if tui.has_active_modal() {
                                    break 'remote_wait;
                                }
                                if !tui.is_any_tab_in_flight() { break 'remote_wait; }
                            }
                            InFlightInterruption::SubmitChatPrompt(prompt) => {
                                let idle_idx = tui.active_tab_index();
                                if !tui.is_any_tab_in_flight() {
                                    tui.set_pending_submission(prompt);
                                    break 'remote_wait;
                                }
                                let eff = cli.build_input_with_qmd_context(&prompt);
                                if cli.spawn_turn_for_tab(idle_idx, eff, cli.permission_mode).is_err() {
                                    // Tab busy — queue onto its own message_queue so it
                                    // dispatches when its current turn ends (TUI-3 path).
                                    tui.enqueue_on_tab(idle_idx, prompt);
                                } else {
                                    tui.set_tab_in_flight(idle_idx, true);
                                }
                            }
                        }
                    }
                    let reaped = cli.try_reap_finished_turns();
                    for (idx, result) in reaped {
                        tui.set_tab_in_flight(idx, false);
                        if let Err(e) = result {
                            if idx == active {
                                tui.push_system(format!("Turn error: {e}"));
                            } else {
                                tui.push_system_to_tab(idx, format!("Turn error: {e}"));
                            }
                        }
                    }
                    // Post-turn work for the active tab.
                    cli.persist_session()?;
                    if let Some(msg) = cli.maybe_auto_compact() {
                        tui.push_system(msg);
                    }
                }
            }
        }

        // Read the next key event (returns quickly with Continue most of the time).
        match tui.read_input()? {
            ReadResult::Continue => {
                // Nothing submitted yet; loop and redraw.
                // Sync active_tab_idx in case a keyboard shortcut (F2/F3,
                // Ctrl+Left/Right, Alt+1-9) switched tabs inside read_input
                // without surfacing a distinct ReadResult variant.
                cli.active_tab_idx = tui.active_tab_index();
                // Drain any pending TUI events from background tabs so their
                // streaming output renders while the user is typing in another tab.
                tui.pump_events_nonblocking();
                // v2.2.14 TUI-2 (deep): reap background workers that finished
                // while the main thread was idle, so the per-tab `in_flight`
                // flag mirrors reality before the next Submit decision (which
                // would otherwise hit "tab already has a turn in flight" and
                // queue when it could have dispatched).
                let reaped = cli.try_reap_finished_turns();
                for (idx, result) in reaped {
                    tui.set_tab_in_flight(idx, false);
                    if let Err(e) = result {
                        tui.push_system_to_tab(idx, format!("Turn error: {e}"));
                    }
                }
            }
            ReadResult::Exit => {
                let drained = cli.drain_all_in_flight_workers();
                if drained > 0 {
                    tui.push_system(format!(
                        "Drained {drained} in-flight turn(s) before exit."
                    ));
                }
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
                let new_session = create_managed_session_handle()?;
                let tab_idx = tui.new_tab("new", cli.model.clone(), new_session.id.clone());
                tui.switch_tab(tab_idx);
                cli.active_tab_idx = tab_idx;
                let tab_id = tui.tab_id_at(tab_idx).unwrap_or(tab_idx + 1);
                let cancel_token = tui
                    .tab_cancel_token(tab_idx)
                    .expect("just-created tab must have a cancel token");
                if let Err(e) = cli.push_tab_runtime(
                    tab_id,
                    &sender_prototype,
                    Session::new(),
                    cli.model.clone(),
                    cli.system_prompt.clone(),
                    true,
                    cli.allowed_tools.clone(),
                    cli.permission_mode,
                    cancel_token,
                ) {
                    tui.push_system(format!("Warning: per-tab runtime failed: {e}"));
                } else {
                    tui.mark_tab_has_runtime(tab_idx);
                }
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
                    let drained = cli.drain_all_in_flight_workers();
                    if drained > 0 {
                        tui.push_system(format!(
                            "Drained {drained} in-flight turn(s) before exit."
                        ));
                    }
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
                            cli.active_tab_idx = tab_idx;
                            let tab_id = tui.tab_id_at(tab_idx).unwrap_or(tab_idx + 1);
                            let cancel_token = tui
                                .tab_cancel_token(tab_idx)
                                .expect("just-created tab must have a cancel token");
                            if let Err(e) = cli.push_tab_runtime(
                                tab_id,
                                &sender_prototype,
                                Session::new(),
                                cli.model.clone(),
                                cli.system_prompt.clone(),
                                true,
                                cli.allowed_tools.clone(),
                                cli.permission_mode,
                                cancel_token,
                            ) {
                                tui.push_system(format!("Warning: per-tab runtime failed: {e}"));
                            } else {
                                tui.mark_tab_has_runtime(tab_idx);
                            }
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
                                let tab_idx = num.saturating_sub(1);
                                tui.switch_tab(tab_idx);
                                cli.active_tab_idx = tui.active_tab_index();
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
                            cli.active_runtime_mut().inject_user_blocks(result.blocks);
                            any_blocks = true;
                        }
                    }
                    if any_blocks {
                        // Run a turn so the model can respond to the injected content.
                        tui.push_system(format!("Thinking... ({})", cli.model));
                        tui.scroll_to_bottom();
                        tui.draw()?;
                        // Blocks are already injected above; spawn the worker to call
                        // run_turn_preloaded without any additional user message.
                        cli.effort_level.apply_to_env();
                        let active = cli.active_tab_idx;
                        let active_tab_id = tui.tab_id_at(active).unwrap_or(active + 1);
                        if let Err(reason) = cli.spawn_file_drop_turn_for_tab(active, cli.permission_mode) {
                            tui.push_system(format!("Cannot start turn: {reason}"));
                        } else {
                            tui.set_tab_in_flight(active, true);
                            // v2.2.14 TUI-2 (deep): wait stays bound to the
                            // tab we just spawned; do NOT rebind on user
                            // actions (see remote_wait above).
                            let wait_tab_id = active_tab_id;
                            'file_wait: loop {
                                match tui.wait_for_turn_end_for_tab(wait_tab_id)? {
                                    InFlightInterruption::TurnDone
                                    | InFlightInterruption::ChannelClosed => break 'file_wait,
                                    InFlightInterruption::TabSwitched => {
                                        cli.active_tab_idx = tui.active_tab_index();
                                        if !tui.is_any_tab_in_flight() { break 'file_wait; }
                                    }
                                    InFlightInterruption::OpenNewTab => {
                                        let new_session = create_managed_session_handle()?;
                                        let tab_idx = tui.new_tab("new", cli.model.clone(), new_session.id.clone());
                                        tui.switch_tab(tab_idx);
                                        cli.active_tab_idx = tab_idx;
                                        let tab_id = tui.tab_id_at(tab_idx).unwrap_or(tab_idx + 1);
                                        let cancel_token = tui
                                            .tab_cancel_token(tab_idx)
                                            .expect("just-created tab must have a cancel token");
                                        if let Err(e) = cli.push_tab_runtime(
                                            tab_id,
                                            &sender_prototype,
                                            Session::new(),
                                            cli.model.clone(),
                                            cli.system_prompt.clone(),
                                            true,
                                            cli.allowed_tools.clone(),
                                            cli.permission_mode,
                                            cancel_token,
                                        ) {
                                            tui.push_system(format!("Warning: per-tab runtime failed: {e}"));
                                        } else {
                                            tui.mark_tab_has_runtime(tab_idx);
                                        }
                                        tui.push_system(format!(
                                            "Opened tab {}  |  session {}",
                                            tab_idx + 1,
                                            new_session.id,
                                        ));
                                    }
                                    InFlightInterruption::CloseActiveTab => {
                                        let idx = tui.active_tab_index();
                                        if idx != active {
                                            tui.close_tab_by_index(idx);
                                            cli.active_tab_idx = tui.active_tab_index();
                                        }
                                    }
                                    InFlightInterruption::SlashCommand(line) => {
                                        let trimmed = line.trim();
                                        if matches!(trimmed, "/exit" | "/quit") {
                                            tui.set_pending_submission(trimmed.to_string());
                                            break 'file_wait;
                                        }
                                        tui.push_system(format!("↳ executing held command: {line}"));
                                        if let Some(command) = SlashCommand::parse(trimmed) {
                                            match cli.handle_repl_command_tui(command, &mut tui) {
                                                Ok(_) => {}
                                                Err(err) => tui.push_system(format!("Error: {err}")),
                                            }
                                            tui.set_thinking_enabled(cli.thinking_enabled);
                                            tui.set_effort_level(cli.effort_level.as_str());
                                        }
                                        if tui.has_active_modal() {
                                            break 'file_wait;
                                        }
                                        if !tui.is_any_tab_in_flight() { break 'file_wait; }
                                    }
                                    InFlightInterruption::SubmitChatPrompt(prompt) => {
                                        let idle_idx = tui.active_tab_index();
                                        if !tui.is_any_tab_in_flight() {
                                            tui.set_pending_submission(prompt);
                                            break 'file_wait;
                                        }
                                        let eff = cli.build_input_with_qmd_context(&prompt);
                                        if cli.spawn_turn_for_tab(idle_idx, eff, cli.permission_mode).is_err() {
                                            tui.enqueue_on_tab(idle_idx, prompt);
                                        } else {
                                            tui.set_tab_in_flight(idle_idx, true);
                                        }
                                    }
                                }
                            }
                            let reaped = cli.try_reap_finished_turns();
                            for (idx, result) in reaped {
                                tui.set_tab_in_flight(idx, false);
                                if let Err(e) = result {
                                    if idx == active {
                                        tui.push_system(format!("Turn error: {e}"));
                                    } else {
                                        tui.push_system_to_tab(idx, format!("Turn error: {e}"));
                                    }
                                }
                            }
                            cli.persist_session()?;
                        }
                        // (held_submit not applicable to file-drop path)
                    }
                    continue;
                }

                // User prompt: run a turn.  The TUI sender is already installed,
                // so model output streams back to the TUI.
                tui.push_system(format!("Thinking... ({})", cli.model));
                tui.scroll_to_bottom();
                tui.draw()?; // Immediate visual feedback before blocking API call

                // Run the turn on a background thread so the main thread can
                // enter the event-drain / animation loop immediately.  This
                // enables live streaming display instead of buffering the full
                // response before showing anything.
                //
                // Preprocessing (instruction hot-reload, pinned files, QMD
                // context) must happen on the main thread because it needs
                // &mut LiveCli.  The blocking API call is offloaded.
                //
                // T1-#400: wait_for_turn_end_for_tab also polls keyboard input
                // during the wait, so the user can compose the next prompt
                // while the current turn streams. If they pressed Enter
                // mid-turn, the captured draft is returned here as
                // `held_submit` and we feed it back into the input loop so
                // it auto-submits as the next turn (no user action needed).
                let mut held_submit: Option<String> = None;
                // Preprocessing on the main thread.
                if cli.maybe_reload_instructions() {
                    let sp = cli.system_prompt.clone();
                    cli.active_runtime_mut().replace_system_prompt(sp);
                }
                cli.inject_pinned_files_for_active_tab();
                cli.effort_level.apply_to_env();
                let effective_input = cli.build_input_with_qmd_context(trimmed);
                let active = cli.active_tab_idx;
                let active_tab_id = tui.tab_id_at(active).unwrap_or(active + 1);
                // v2.2.14 TUI-2 (deep): if the active tab is itself still
                // streaming a background turn spawned earlier (e.g. via
                // SubmitChatPrompt on an idle tab during a previous wait),
                // queue the user's prompt onto that tab's message_queue so
                // it fires when the current turn finishes (TUI-3 path)
                // instead of being dropped with "tab already in flight".
                let active_busy = cli.tab_runtimes
                    .get(active)
                    .is_some_and(|t| t.in_flight.is_some());
                if active_busy {
                    tui.enqueue_on_tab(active, trimmed);
                    tui.push_system("↳ queued: tab still finishing previous turn".to_string());
                    continue;
                }
                if let Err(reason) = cli.spawn_turn_for_tab(active, effective_input, cli.permission_mode) {
                    tui.push_system(format!("Cannot start turn: {reason}"));
                } else {
                    tui.set_tab_in_flight(active, true);
                    // v2.2.14 TUI-2 (deep): wait stays bound to the originally
                    // spawned tab. Background turns spawned from
                    // SubmitChatPrompt are reaped here after the wait or in
                    // the read_input Continue path; the user's draft on an
                    // idle background tab dispatches IMMEDIATELY (no longer
                    // gated behind the original target tab's TurnDone).
                    let wait_tab_id = active_tab_id;
                    'chat_wait: loop {
                        match tui.wait_for_turn_end_for_tab(wait_tab_id)? {
                            InFlightInterruption::TurnDone
                            | InFlightInterruption::ChannelClosed => {
                                // Pick up any type-ahead draft the user committed
                                // while the turn was in flight.
                                held_submit = tui.pending_submit.take();
                                break 'chat_wait;
                            }
                            InFlightInterruption::TabSwitched => {
                                cli.active_tab_idx = tui.active_tab_index();
                                if !tui.is_any_tab_in_flight() {
                                    held_submit = tui.pending_submit.take();
                                    break 'chat_wait;
                                }
                            }
                            InFlightInterruption::OpenNewTab => {
                                let new_session = create_managed_session_handle()?;
                                let tab_idx = tui.new_tab("new", cli.model.clone(), new_session.id.clone());
                                tui.switch_tab(tab_idx);
                                cli.active_tab_idx = tab_idx;
                                let tab_id = tui.tab_id_at(tab_idx).unwrap_or(tab_idx + 1);
                                let cancel_token = tui
                                    .tab_cancel_token(tab_idx)
                                    .expect("just-created tab must have a cancel token");
                                if let Err(e) = cli.push_tab_runtime(
                                    tab_id,
                                    &sender_prototype,
                                    Session::new(),
                                    cli.model.clone(),
                                    cli.system_prompt.clone(),
                                    true,
                                    cli.allowed_tools.clone(),
                                    cli.permission_mode,
                                    cancel_token,
                                ) {
                                    tui.push_system(format!("Warning: per-tab runtime failed: {e}"));
                                } else {
                                    tui.mark_tab_has_runtime(tab_idx);
                                }
                                tui.push_system(format!(
                                    "Opened tab {}  |  session {}",
                                    tab_idx + 1,
                                    new_session.id,
                                ));
                            }
                            InFlightInterruption::CloseActiveTab => {
                                let idx = tui.active_tab_index();
                                if idx != active {
                                    tui.close_tab_by_index(idx);
                                    cli.active_tab_idx = tui.active_tab_index();
                                }
                            }
                            InFlightInterruption::SlashCommand(line) => {
                                let cmd_trimmed = line.trim();
                                if matches!(cmd_trimmed, "/exit" | "/quit") {
                                    tui.set_pending_submission(cmd_trimmed.to_string());
                                    break 'chat_wait;
                                }
                                tui.push_system(format!("↳ executing held command: {line}"));
                                if let Some(command) = SlashCommand::parse(cmd_trimmed) {
                                    match cli.handle_repl_command_tui(command, &mut tui) {
                                        Ok(_) => {}
                                        Err(err) => tui.push_system(format!("Error: {err}")),
                                    }
                                    tui.set_thinking_enabled(cli.thinking_enabled);
                                    tui.set_effort_level(cli.effort_level.as_str());
                                }
                                if tui.has_active_modal() {
                                    break 'chat_wait;
                                }
                                if !tui.is_any_tab_in_flight() {
                                    break 'chat_wait;
                                }
                            }
                            InFlightInterruption::SubmitChatPrompt(prompt) => {
                                let idle_idx = tui.active_tab_index();
                                if !tui.is_any_tab_in_flight() {
                                    // No turn running — stash as pending and exit.
                                    tui.set_pending_submission(prompt);
                                    break 'chat_wait;
                                }
                                let eff = cli.build_input_with_qmd_context(&prompt);
                                if cli.spawn_turn_for_tab(idle_idx, eff, cli.permission_mode).is_err() {
                                    // Tab is itself in-flight — queue onto its
                                    // own message_queue so it fires when its
                                    // current turn finishes (TUI-3 path).
                                    tui.enqueue_on_tab(idle_idx, prompt);
                                } else {
                                    tui.set_tab_in_flight(idle_idx, true);
                                }
                            }
                        }
                    }
                    // Reap finished handles (active tab finished; background tabs
                    // may still be running and will be reaped on future iterations).
                    let reaped = cli.try_reap_finished_turns();
                    for (idx, result) in reaped {
                        tui.set_tab_in_flight(idx, false);
                        if let Err(e) = result {
                            if idx == active {
                                tui.push_system(format!("Turn error: {e}"));
                            } else {
                                tui.push_system_to_tab(idx, format!("Turn error: {e}"));
                            }
                        }
                    }
                    // Post-turn work for the active tab: persist session,
                    // auto-compact, notification hooks, skill hints.
                    cli.persist_session()?;
                    if let Some(msg) = cli.maybe_auto_compact() {
                        tui.push_system(msg);
                    }
                    // Notification hooks.
                    cli.active_runtime_mut().run_notification_hooks(&NotificationPayload {
                        kind: NotificationKind::Completion,
                        message: "Turn complete".to_string(),
                    });
                    // Skill suggestion hints.
                    {
                        use commands::agents::{discover_skill_roots, load_skills_from_roots};
                        use commands::{ChainEvaluator, format_chain_hint};
                        let cwd = std::env::current_dir().unwrap_or_default();
                        let roots = discover_skill_roots(&cwd);
                        if let Ok(skills) = load_skills_from_roots(&roots) {
                            let skills_with_triggers: Vec<&commands::agents::SkillSummary> =
                                skills.iter().filter(|s| !s.triggers.is_empty()).collect();
                            let matches = match_triggers(trimmed, &skills_with_triggers);
                            if let Some(hint) = format_suggestions_hint(&matches) {
                                tui.push_system(hint);
                            }
                            let loaded: Vec<commands::agents::SkillSummary> = cli.loaded_skills_snapshot();
                            if !loaded.is_empty() {
                                let all_skills: std::collections::HashMap<String, commands::agents::SkillSummary> =
                                    skills.into_iter().map(|s| (s.name.to_ascii_lowercase(), s)).collect();
                                let evaluator = ChainEvaluator::new();
                                let candidates = evaluator.evaluate(&loaded, &all_skills, trimmed);
                                for loaded_skill in &loaded {
                                    let skill_candidates: Vec<_> = candidates.iter()
                                        .filter(|c| c.triggered_by.eq_ignore_ascii_case(&loaded_skill.name))
                                        .cloned()
                                        .collect();
                                    if let Some(hint) = format_chain_hint(&loaded_skill.name, &skill_candidates) {
                                        tui.push_system(hint);
                                    }
                                }
                            }
                        }
                    }
                }
                // T1-#400: if user pressed Enter during the in-flight turn,
                // we held the draft. Behavior depends on shape:
                //   - Slash command (starts with '/'): auto-fire as the next
                //     submission. Anyone typing `/ssh` mid-turn meant to
                //     EXECUTE that command, not enqueue it as a chat message.
                //     We stash it as a synthetic "next submit" — the main
                //     read_input loop's first iteration picks it up and
                //     dispatches through the normal slash path.
                //   - Free text: drop back into the input box as a draft so
                //     the user can cancel-on-misfire or edit before sending.
                //     This is the original v2.2.12 behavior, preserved.
                if let Some(draft) = held_submit
                    && !draft.is_empty()
                {
                    // v2.2.14 TUI-3: held drafts (whether slash commands or
                    // free text) auto-dispatch on the next read_input pass.
                    // The user explicitly queued them while the prior turn
                    // streamed; making them re-press Enter would defeat
                    // the queueing UX. After dispatch, the next-in-queue is
                    // promoted to `pending_submit` for the following turn.
                    if draft.starts_with('/') {
                        tui.push_system(format!("↳ executing held command: {draft}"));
                    } else {
                        tui.push_system(format!("↳ dispatching held message: {draft}"));
                    }
                    tui.set_pending_submission(draft);
                    // Promote the next queued message (if any) into
                    // pending_submit so the in-flight handler picks it up
                    // for the upcoming turn.
                    tui.promote_next_queued_for_active();
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

    // Defense-in-depth: every `break 'outer` path above already drains
    // in-flight workers, but if a new exit path is added that forgets,
    // this final drain prevents the SessionEnd hooks lock from deadlocking
    // against a worker that still holds the runtime mutex.
    let _ = cli.drain_all_in_flight_workers();

    // v2.2.11: fire SessionEnd hooks on clean exit.
    let _ = cli.active_runtime_mut().run_session_end_hooks();

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
                            cli.active_runtime_mut().inject_user_blocks(result.blocks);
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
        cli.active_runtime_mut().inject_user_message(&notification);
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
        cli.active_runtime_mut().inject_user_message(&notification);
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

/// Per-tab runtime state (bug 3 — per-tab parallel inference).
///
/// Each `Tab` in `AnvilTui.tabs[i]` has a corresponding `TabRuntimeState` at
/// `LiveCli.tab_runtimes[i]`. The runtime and its per-tab TuiSenderSlot are
/// kept here (not in `tui::state::Tab`) to avoid a circular-dependency between
/// the `tui` submodule and `providers.rs`.
///
/// The runtime is wrapped in `Arc<Mutex<...>>` so that a background worker
/// thread can hold a clone of the Arc and lock only the index it owns while
/// the main thread continues to service other tabs. This is Strategy A from
/// the Commit 3 spec.
struct TabRuntimeState {
    /// The per-tab conversation runtime, behind a mutex so a background
    /// worker thread can run a turn without holding `&mut LiveCli`.
    runtime: Arc<Mutex<ConversationRuntime<DefaultRuntimeClient, CliToolExecutor>>>,
    /// Per-tab sender slot. Each tab's runtime holds a `TuiSenderSlot` whose
    /// inner `TuiSender` is stamped with that tab's `id`, so events always
    /// route to the correct tab regardless of which tab is active.
    tui_slot: TuiSenderSlot,
    /// Handle to the background worker running the current turn on this tab,
    /// if any. `None` means the tab is idle. The handle is set by
    /// `spawn_turn_for_tab` and reaped by `try_reap_finished_turns`.
    in_flight: Option<std::thread::JoinHandle<Result<(), String>>>,
}

struct LiveCli {
    model: String,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
    /// Typed working-memory layer (v2.2.14 Phase 1 Bucket 1.1): the in-memory
    /// prompt is `Vec<PromptSection>` so commands like `/goal`,
    /// `/output-style`, `/fast`, and `/skill load` can identify their sections
    /// by kind. The wire format is projected to `Vec<String>` only at the
    /// API boundary in `providers.rs`.
    system_prompt: Vec<runtime::PromptSection>,
    /// Per-tab runtimes (bug 3). Index i corresponds to AnvilTui.tabs[i].
    /// Always non-empty; index 0 is the bootstrap tab's runtime.
    tab_runtimes: Vec<TabRuntimeState>,
    /// Which tab index is currently active. Kept in sync with
    /// `AnvilTui.active_tab` by the run loop after every tab switch.
    active_tab_idx: usize,
    session: SessionHandle,
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
        // W15b: install the auto-promote engine once per process. Wires
        // read_file / bash observations into the nominations queue.
        runtime::install_auto_promote_default();

        let provider = friendly_provider_label(&model);
        let system_prompt = build_system_prompt_with_identity(
            Some(model.clone()),
            provider,
            None,
        )?;
        let session = create_managed_session_handle()?;
        let bootstrap_tui_slot: TuiSenderSlot = Arc::new(Mutex::new(None));
        // Shared agent manager — owned here for TUI polling, shared with
        // CliToolExecutor so tool calls can register real agent threads.
        let agent_manager: Arc<Mutex<agents::AgentManager>> =
            Arc::new(Mutex::new(agents::AgentManager::new()));
        let bootstrap_runtime = build_runtime_with_tui_slot(
            Session::new(),
            model.clone(),
            system_prompt.clone(),
            enable_tools,
            true,
            allowed_tools.clone(),
            permission_mode,
            None,
            bootstrap_tui_slot.clone(),
            agent_manager.clone(),
        )?;
        let qmd = QmdClient::new();
        let history_archiver = HistoryArchiver::new();
        // Phase 4.1 (L2 §4): auto-prune retention on session start.
        // Best-effort: move history files older than
        // ANVIL_HISTORY_RETENTION_DAYS (default 90) into ~/.anvil/.trash/
        // and permanently delete trash items past their secondary 30-day
        // window. Capped at MAX_AUTO_PRUNE_MOVES (100) per session so a
        // fleet of old archives can't stall startup.
        let prune_summary = history_archiver.auto_prune_on_session_start();
        if let Some(line) = prune_summary.format_one_line() {
            eprintln!("{line}");
        }
        // Best-effort: register and refresh the anvil-history QMD collection.
        qmd.ensure_history_indexed(history_archiver.history_dir());
        // Phase 3.2: also register the anvil-semantic collection used by
        // /memory promote. Idempotent + silent-on-failure (no qmd binary).
        let _ = runtime::qmd::ensure_semantic_collection();
        let cli = Self {
            model,
            allowed_tools,
            permission_mode,
            system_prompt,
            tab_runtimes: vec![TabRuntimeState {
                runtime: Arc::new(Mutex::new(bootstrap_runtime)),
                tui_slot: bootstrap_tui_slot,
                in_flight: None,
            }],
            active_tab_idx: 0,
            session,
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
        // Publish initial cross-session snapshot now that the session id is
        // known.  Subsequent updates flow from AgentManager::spawn/poll.
        // (CC-139-F1, #462.)
        {
            let session_id = cli.session.id.clone();
            if let Ok(mut mgr) = cli.agent_manager.lock() {
                mgr.set_session_id(session_id);
            }
        }
        cli.persist_session()?;
        // Emit session_start OTel event (no-op when disabled).
        {
            let _detected_provider_kind = api::detect_provider_kind(&cli.model);
            let provider_name: &str = match _detected_provider_kind {
                api::ProviderKind::AnvilApi => "anthropic",
                api::ProviderKind::Xai => "xai",
                api::ProviderKind::OpenAi => "openai",
                api::ProviderKind::Gemini => "gemini",
                api::ProviderKind::Ollama => "ollama",
                api::ProviderKind::Fireworks => "fireworks",
                api::ProviderKind::MiniMax => "minimax",
                api::ProviderKind::Groq => "groq",
                api::ProviderKind::Mistral => "mistral",
                api::ProviderKind::Perplexity => "perplexity",
                api::ProviderKind::DeepSeek => "deepseek",
                api::ProviderKind::TogetherAi => "togetherai",
                api::ProviderKind::DeepInfra => "deepinfra",
                api::ProviderKind::Chutes => "chutes",
                api::ProviderKind::Cerebras => "cerebras",
                api::ProviderKind::NvidiaNim => "nvidia-nim",
                api::ProviderKind::HuggingFace => "huggingface",
                api::ProviderKind::MoonshotAi => "moonshotai",
                api::ProviderKind::Nebius => "nebius",
                api::ProviderKind::Scaleway => "scaleway",
                api::ProviderKind::StackIt => "stackit",
                api::ProviderKind::Baseten => "baseten",
                api::ProviderKind::Cortecs => "cortecs",
                api::ProviderKind::Ai302 => "302ai",
                api::ProviderKind::Zai => "zai",
                api::ProviderKind::OpenRouter => "openrouter",
                api::ProviderKind::LmStudio => "lmstudio",
                api::ProviderKind::OpenCode => "opencode",
                api::ProviderKind::OpenCodeGo => "opencode-go",
                api::ProviderKind::Copilot => "copilot",
                api::ProviderKind::Azure => "azure",
                api::ProviderKind::Bedrock => "bedrock",
                api::ProviderKind::Alibaba => "alibaba",
                api::ProviderKind::Antigravity => "antigravity",
                api::ProviderKind::Cursor => "cursor",
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

    // ── Per-tab runtime accessors (bug 3) ────────────────────────────────────

    /// Get an immutable reference to the active tab's runtime.
    ///
    /// Lock and return the active tab's runtime.
    ///
    /// Returns a `MutexGuard` that derefs to `&ConversationRuntime`.  The lock
    /// is released when the guard is dropped.  Callers that hold the guard
    /// across a `spawn_turn_for_tab` call would deadlock — don't do that.
    ///
    /// Panics if the active tab has no runtime installed, which should never
    /// happen after `LiveCli::new` completes.
    fn active_runtime(
        &self,
    ) -> std::sync::MutexGuard<'_, ConversationRuntime<DefaultRuntimeClient, CliToolExecutor>> {
        self.tab_runtimes[self.active_tab_idx]
            .runtime
            .lock()
            .expect("runtime mutex poisoned")
    }

    /// Lock and return the active tab's runtime mutably.
    ///
    /// Same lifetime / deadlock caveat as `active_runtime`.
    fn active_runtime_mut(
        &self,
    ) -> std::sync::MutexGuard<'_, ConversationRuntime<DefaultRuntimeClient, CliToolExecutor>> {
        self.tab_runtimes[self.active_tab_idx]
            .runtime
            .lock()
            .expect("runtime mutex poisoned")
    }

    /// Install a new runtime for the active tab, replacing any existing one.
    fn install_active_runtime(
        &mut self,
        rt: ConversationRuntime<DefaultRuntimeClient, CliToolExecutor>,
    ) {
        *self.tab_runtimes[self.active_tab_idx]
            .runtime
            .lock()
            .expect("runtime mutex poisoned") = rt;
    }

    /// Spawn a model turn for the given tab index.  Returns immediately; the
    /// turn runs in a background thread and emits `TaggedTuiEvent`s through
    /// the tab's pre-installed `TuiSender`.
    ///
    /// `effective_input` is the already-preprocessed prompt (QMD context
    /// injected, etc.).  The caller must do preprocessing on the main thread
    /// before calling this.
    ///
    /// The worker thread calls `ConversationRuntime::run_turn` (which streams
    /// `TextDelta` events), then sends `Tokens` and `TurnDone` via the tab's
    /// `TuiSender`.  The caller must call `wait_for_turn_end_for_tab` to wait
    /// for the `TurnDone` before reading results.
    ///
    /// Stores the `JoinHandle` in `tab_runtimes[tab_idx].in_flight` so the
    /// main loop can reap it via `try_reap_finished_turns`.
    ///
    /// Returns `Err` if a turn is already in flight on this tab.
    fn spawn_turn_for_tab(
        &mut self,
        tab_idx: usize,
        effective_input: String,
        permission_mode: PermissionMode,
    ) -> Result<(), String> {
        if self.tab_runtimes[tab_idx].in_flight.is_some() {
            return Err(format!("tab {} already has a turn in flight", tab_idx + 1));
        }
        let runtime_arc = Arc::clone(&self.tab_runtimes[tab_idx].runtime);
        // Grab a TuiSender clone so the worker can emit TurnDone.
        let tui_sender: Option<TuiSender> = self.tab_runtimes[tab_idx]
            .tui_slot
            .lock()
            .ok()
            .and_then(|g| g.clone());

        // CC parity v2.2.14: capture per-session env for the worker thread to
        // install via session_ctx::set(). The thread-local partitions across
        // parallel tabs (see #433 per-tab inference); each spawned thread
        // sees its own snapshot. Source values from LiveCli at spawn time:
        // session_id is the active session ID, effort_level reflects the
        // current /effort choice, project_dir is the process cwd.
        //
        // Known imperfection: for tab spawns that target a non-active tab
        // (e.g., remote-control submission to tab 2 while tab 1 is active),
        // the session_id reflects the active session rather than the target
        // tab's. Matching CC's single-session env behavior is acceptable
        // here — CC has no per-tab parallelism at all.
        let session_ctx_snapshot = runtime::session_ctx::SessionContext {
            session_id: self.session_id().to_string(),
            effort_level: self.effort_level.as_str().to_string(),
            project_dir: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        };

        let handle = std::thread::spawn(move || -> Result<(), String> {
            runtime::session_ctx::set(session_ctx_snapshot);
            let mut rt = runtime_arc
                .lock()
                .map_err(|_| "runtime mutex poisoned".to_string())?;
            // Bug-3 Commit 4: wire the per-tab TuiSender into the prompter so
            // permission decisions go through the TUI modal rather than
            // blocking stdin.
            let mut prompter = CliPermissionPrompter::new(permission_mode);
            if let Some(sender) = tui_sender.clone() {
                prompter = prompter.with_tui_sender(sender);
            }
            // Surface the thinking indicator on the target tab's TUI sender so
            // background-tab spawns get the same "Thinking..." line that the
            // active-tab path emits. Without this, a turn dispatched from a
            // tab the user just switched to runs silently until the first
            // TextDelta arrives.
            if let Some(ref tx) = tui_sender {
                tx.send(TuiEvent::ThinkLabel("Thinking...".to_string()));
            }
            match rt.run_turn(&effective_input, Some(&mut prompter)) {
                Ok(ref summary) => {
                    if let Some(ref tx) = tui_sender {
                        let usage = summary.usage;
                        tx.send(TuiEvent::Tokens {
                            input: usage.input_tokens,
                            output: usage.output_tokens,
                        });
                        tx.send(TuiEvent::TurnDone);
                    }
                    Ok(())
                }
                Err(e) => {
                    if let Some(ref tx) = tui_sender {
                        tx.send(TuiEvent::System(format!("Error: {e}")));
                        tx.send(TuiEvent::TurnDone);
                    }
                    Err(e.to_string())
                }
            }
        });
        self.tab_runtimes[tab_idx].in_flight = Some(handle);
        Ok(())
    }

    /// Spawn a file-drop turn for the given tab index.
    ///
    /// Blocks have already been injected via `inject_user_blocks`.  The worker
    /// calls `run_turn_preloaded` and emits `TurnDone` via the tab's sender.
    fn spawn_file_drop_turn_for_tab(
        &mut self,
        tab_idx: usize,
        permission_mode: PermissionMode,
    ) -> Result<(), String> {
        if self.tab_runtimes[tab_idx].in_flight.is_some() {
            return Err(format!("tab {} already has a turn in flight", tab_idx + 1));
        }
        let runtime_arc = Arc::clone(&self.tab_runtimes[tab_idx].runtime);
        let tui_sender: Option<TuiSender> = self.tab_runtimes[tab_idx]
            .tui_slot
            .lock()
            .ok()
            .and_then(|g| g.clone());

        // CC parity v2.2.14: same per-session env propagation as
        // spawn_turn_for_tab — keep this branch in sync if you change either.
        let session_ctx_snapshot = runtime::session_ctx::SessionContext {
            session_id: self.session_id().to_string(),
            effort_level: self.effort_level.as_str().to_string(),
            project_dir: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        };

        let handle = std::thread::spawn(move || -> Result<(), String> {
            runtime::session_ctx::set(session_ctx_snapshot);
            let mut rt = runtime_arc
                .lock()
                .map_err(|_| "runtime mutex poisoned".to_string())?;
            // Bug-3 Commit 4: wire per-tab TuiSender for modal prompts.
            let mut prompter = CliPermissionPrompter::new(permission_mode);
            if let Some(sender) = tui_sender.clone() {
                prompter = prompter.with_tui_sender(sender);
            }
            // Surface the thinking indicator on the target tab's TUI sender so
            // background-tab preloaded spawns get the same "Thinking..." line
            // that the active-tab path emits.
            if let Some(ref tx) = tui_sender {
                tx.send(TuiEvent::ThinkLabel("Thinking...".to_string()));
            }
            match rt.run_turn_preloaded(Some(&mut prompter)) {
                Ok(ref summary) => {
                    if let Some(ref tx) = tui_sender {
                        let usage = summary.usage;
                        tx.send(TuiEvent::Tokens {
                            input: usage.input_tokens,
                            output: usage.output_tokens,
                        });
                        tx.send(TuiEvent::TurnDone);
                    }
                    Ok(())
                }
                Err(e) => {
                    if let Some(ref tx) = tui_sender {
                        tx.send(TuiEvent::System(format!("Error: {e}")));
                        tx.send(TuiEvent::TurnDone);
                    }
                    Err(e.to_string())
                }
            }
        });
        self.tab_runtimes[tab_idx].in_flight = Some(handle);
        Ok(())
    }

    /// Poll all tabs for finished turns; join and return results.
    ///
    /// Non-blocking: returns only the handles that report `is_finished()`.
    /// Each entry is `(tab_idx, Ok(()) | Err(msg))`.
    fn try_reap_finished_turns(&mut self) -> Vec<(usize, Result<(), String>)> {
        let mut results = Vec::new();
        for (idx, tab_rt) in self.tab_runtimes.iter_mut().enumerate() {
            if tab_rt
                .in_flight
                .as_ref()
                .is_some_and(|h| h.is_finished())
            {
                let handle = tab_rt.in_flight.take().expect("just confirmed Some");
                let result = handle
                    .join()
                    .unwrap_or_else(|_| Err("worker thread panicked".to_string()));
                results.push((idx, result));
            }
        }
        results
    }

    /// Blocking-join every in-flight worker.
    ///
    /// Must be called before any code path that locks a tab's runtime mutex
    /// from the main thread on exit (persist_session, record_daily,
    /// run_session_end_hooks). A worker holds its tab's runtime mutex for the
    /// duration of a turn; calling lock() while a worker is mid-turn deadlocks
    /// the main thread — which is what /quit was doing before this helper.
    ///
    /// Returns the number of workers that were drained.
    fn drain_all_in_flight_workers(&mut self) -> usize {
        let mut drained = 0;
        for tab_rt in self.tab_runtimes.iter_mut() {
            if let Some(handle) = tab_rt.in_flight.take() {
                let _ = handle.join();
                drained += 1;
            }
        }
        drained
    }

    /// Inject any pinned files as user messages into the active tab's runtime.
    ///
    /// Must be called on the main thread before spawning a turn, because it
    /// mutates the runtime (adds messages to the conversation history).
    fn inject_pinned_files_for_active_tab(&mut self) {
        if let Ok(pinned_path) = anvil_pinned_path()
            && let Ok(pinned) = load_pinned_paths(&pinned_path)
        {
            for path in &pinned {
                if let Ok(content) = fs::read_to_string(path) {
                    let reminder = format!(
                        "<system-reminder>Pinned file context: {}\n{}</system-reminder>",
                        path.display(),
                        content
                    );
                    self.active_runtime_mut().inject_user_message(&reminder);
                }
            }
        }
    }

    /// Get a clone of the active tab's `TuiSenderSlot`.
    fn active_tui_slot(&self) -> TuiSenderSlot {
        self.tab_runtimes[self.active_tab_idx].tui_slot.clone()
    }

    /// Install a TUI sender into the active tab's slot so model/tool output
    /// goes to the TUI for that tab.
    fn enable_tui(&self, sender: TuiSender) {
        if let Ok(mut guard) = self.tab_runtimes[self.active_tab_idx].tui_slot.lock() {
            *guard = Some(sender);
        }
    }

    /// Remove the active tab's TUI sender (fallback to stdout).
    #[allow(dead_code)]
    fn disable_tui(&self) {
        if let Ok(mut guard) = self.tab_runtimes[self.active_tab_idx].tui_slot.lock() {
            *guard = None;
        }
    }

    /// Push a new `TabRuntimeState` for a freshly opened tab and return its
    /// index.  The new slot is immediately enabled with `sender` stamped for
    /// `tab_id`.  Called from the `/tab new` handler and the remote-control
    /// `__new_tab:` message path.
    ///
    /// `cancel_token` (v2.2.14 TUI-1) is installed onto the runtime so Ctrl+C
    /// in the TUI's in-flight handler cancels the streaming turn.
    #[allow(clippy::too_many_arguments)]
    fn push_tab_runtime(
        &mut self,
        tab_id: usize,
        sender_prototype: &TuiSender,
        session: Session,
        model: String,
        system_prompt: Vec<runtime::PromptSection>,
        enable_tools: bool,
        allowed_tools: Option<AllowedToolSet>,
        permission_mode: PermissionMode,
        cancel_token: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        let new_slot: TuiSenderSlot = Arc::new(Mutex::new(None));
        // Pre-install a sender stamped with the new tab's id.
        if let Ok(mut guard) = new_slot.lock() {
            *guard = Some(sender_prototype.with_tab_id(tab_id));
        }
        let mut rt = build_runtime_with_tui_slot(
            session,
            model,
            system_prompt,
            enable_tools,
            true,
            allowed_tools,
            permission_mode,
            None,
            new_slot.clone(),
            self.agent_manager.clone(),
        )?;
        rt.set_cancel_handle(cancel_token);
        let idx = self.tab_runtimes.len();
        self.tab_runtimes.push(TabRuntimeState {
            runtime: Arc::new(Mutex::new(rt)),
            tui_slot: new_slot,
            in_flight: None,
        });
        Ok(idx)
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
            .active_tui_slot()
            .lock()
            .ok()
            .and_then(|guard| guard.clone());

        if let Some(tx) = tui_tx {
            tx.send(TuiEvent::ThinkLabel("Thinking...".to_string()));
            let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
            let result = self
                .active_runtime_mut()
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
                .active_runtime_mut()
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
            // Clone before calling active_runtime_mut() to avoid an immutable
            // borrow of self.system_prompt conflicting with the mutable borrow.
            let sp = self.system_prompt.clone();
            self.active_runtime_mut().replace_system_prompt(sp);
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
                        self.active_runtime_mut().inject_user_message(&reminder);
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
            .active_tui_slot()
            .lock()
            .ok()
            .and_then(|guard| guard.clone());

        if let Some(tx) = tui_tx {
            // TUI path: send thinking indicator update, run turn, send TurnDone.
            tx.send(TuiEvent::ThinkLabel("Thinking...".to_string()));
            let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
            let result = self
                .active_runtime_mut()
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
                    self.active_runtime_mut().run_notification_hooks(&NotificationPayload {
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
                    self.active_runtime_mut().run_notification_hooks(&NotificationPayload {
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
                .active_runtime_mut()
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
                // Live `/model <name>` in the TUI: route through the
                // shared apply_model_switch helper so API routing,
                // system-prompt identity, and TUI chrome all flip
                // together. Without this the chrome updated but
                // inference kept hitting the previous provider.
                //
                // For bare model names (no "/" prefix), check the model
                // choices cache for ambiguity: if >1 providers expose the
                // same bare model ID, surface an error with qualified options
                // rather than silently routing to the heuristic winner.
                if !new_model.contains('/') {
                    // Build a UnifiedModel catalog from the picker cache so we
                    // can call resolve_model_switch without a network fetch.
                    let cache_models = crate::tui::completion::model_choices_cache_snapshot();
                    if !cache_models.is_empty() {
                        // Cache is in "(provider_slug/model_id, label)" form.
                        let catalog: Vec<api::UnifiedModel> = cache_models
                            .iter()
                            .filter_map(|(prefixed, _label)| {
                                let (slug, model_id) = prefixed.split_once('/')?;
                                let provider = api::slug_to_provider_kind(slug)?;
                                Some(api::UnifiedModel {
                                    provider,
                                    model_id: model_id.to_string(),
                                    display: prefixed.clone(),
                                })
                            })
                            .collect();
                        match api::resolve_model_switch(new_model, &catalog) {
                            api::ModelSwitchResolution::Ambiguous { model_id, providers } => {
                                tui.push_system(api::format_ambiguous_model_error(
                                    &model_id,
                                    &providers,
                                ));
                                return Ok(false);
                            }
                            api::ModelSwitchResolution::NotFound { .. } => {
                                // Let apply_model_switch fall through — it may
                                // still succeed via detect_provider_kind heuristics
                                // for models that aren't in the picker cache yet.
                            }
                            api::ModelSwitchResolution::Resolved { .. } => {
                                // Unambiguous — fall through to apply_model_switch.
                            }
                        }
                    }
                }
                match self.apply_model_switch(new_model, Some(tui)) {
                    Ok((previous, msg_count)) => {
                        if previous == self.model {
                            // Same-model no-op — surface a tidy report
                            // rather than the "switched" template.
                            tui.push_system(format_model_report(
                                &self.model,
                                msg_count,
                                self.active_runtime().usage().turns(),
                            ));
                        } else {
                            tui.push_system(format_model_switch_report(
                                &previous,
                                &self.model,
                                msg_count,
                            ));
                        }
                    }
                    Err(err) => {
                        tui.push_system(format!("Model switch failed: {err}"));
                    }
                }
                return Ok(false);
            }
            SlashCommand::GenerateImage { prompt, wp_post_id } => {
                // Image generation takes 10-30 seconds — temporarily leave the alternate
                // screen so the user sees progress output directly on their terminal.
                let _ = terminal::disable_raw_mode();
                crate::tui::leave_alt_screen_for_inline_op();
                println!();
                let result = self.run_generate_image(prompt, wp_post_id.as_deref());
                println!("{result}");
                print!("\nPress Enter to return to Anvil… ");
                let _ = io::stdout().flush();
                let mut buf = String::new();
                let _ = io::stdin().read_line(&mut buf);
                let _ = terminal::enable_raw_mode();
                crate::tui::restore_alt_screen();
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
            SlashCommand::Ssh { args } => {
                // T5-Ssh-F: /ssh end-to-end wiring.
                // - bare `/ssh`         → open the SSH form modal
                // - `/ssh <alias>`      → load alias from vault and prefill form
                // - `/ssh save <alias>` → save active SSH tab's config to vault
                // - `/ssh list`         → list saved aliases
                self.handle_ssh_tui_command(args.as_deref(), tui)?;
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
            // v2.2.16: live layout switch — needs tui access, so handled here.
            SlashCommand::Layout { action } => {
                let msg = self.handle_layout_command(action.as_deref(), tui);
                tui.push_system(msg);
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
                let cumulative = self.active_runtime().usage().cumulative_usage();
                let latest = self.active_runtime().usage().current_turn_usage();
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
                        message_count: self.active_runtime().session().messages.len(),
                        turns: self.active_runtime().usage().turns(),
                        latest,
                        cumulative,
                        estimated_tokens: self.active_runtime().estimated_tokens(),
                    },
                    self.permission_mode.as_str(),
                    &ctx,
                ), false)
            }
            SlashCommand::Cost => {
                let c = self.active_runtime().usage().cumulative_usage();
                (format!("Tokens: ↑{} ↓{} (total: {})", c.input_tokens, c.output_tokens, c.input_tokens + c.output_tokens), false)
            }
            SlashCommand::Version => {
                (format!("Anvil CLI v{VERSION}\nBuild: {BUILD_TARGET} / {GIT_SHA}"), false)
            }
            SlashCommand::Config { section } => {
                let report = render_config_report(section.as_deref())?;
                (report, false)
            }
            SlashCommand::Memory { action } => {
                // Phase 2 / Bucket 2 / L1 §4-5: dispatch `/memory <action>` to
                // the shared handler with a live working-memory snapshot so
                // `show working`, `why`, and `budget` can introspect the
                // actual system_prompt instead of static text. The pure
                // read-side handler runs on the TUI input thread; no LLM
                // calls, no network IO — instant by construction (191cf16).
                let runtime = self.active_runtime();
                let snapshot = runtime.working_memory_snapshot();
                let msg_count = runtime.session().messages.len();
                let msg_tokens = runtime.estimated_tokens();
                drop(runtime);
                let ctx = commands::MemoryContext::with_working(
                    &snapshot,
                    msg_count,
                    msg_tokens,
                );
                (commands::handle_memory_command(action.as_deref(), &ctx), false)
            }
            SlashCommand::Ollama { args } => {
                let ollama_host = std::env::var("OLLAMA_HOST")
                    .unwrap_or_else(|_| "http://localhost:11434".to_string());
                let out = crate::ollama_cmds::run_ollama_command(args.as_deref(), &ollama_host);
                (out, false)
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
                        self.active_runtime().session().messages.len(),
                        self.active_runtime().usage().turns(),
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
                    if let Some(tx) = self.active_tui_slot().lock().ok().and_then(|g| g.clone()) {
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
            // T5-Ssh: embedded SSH client — REPL (non-TUI) fallback only.
            // TUI wiring is handled in handle_repl_command_tui above.
            // In the plain REPL we just explain that SSH needs the TUI.
            SlashCommand::Ssh { args: _ } => {
                ("/ssh requires the TUI — run `anvil` without --print to use the embedded SSH client.".to_string(), false)
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
                                // The composed trait body is a single text fragment.
                                // Tag it as Custom so it survives serialization but
                                // sits at the top of the prompt, mirroring the old
                                // "insert(0, composed.prompt)" behavior.
                                let mut composed_system_prompt: Vec<runtime::PromptSection> = vec![
                                    runtime::PromptSection::new(
                                        runtime::PromptSectionKind::Custom,
                                        composed.prompt.clone(),
                                    ),
                                ];
                                composed_system_prompt.extend(original_system_prompt.iter().cloned());

                                let rebuild = build_runtime_with_tui_slot(
                                    self.active_runtime().session().clone(),
                                    self.model.clone(),
                                    composed_system_prompt,
                                    true,
                                    true,
                                    self.allowed_tools.clone(),
                                    self.permission_mode,
                                    None,
                                    self.active_tui_slot(),
                                    self.agent_manager.clone(),
                                );
                                match rebuild {
                                    Err(e) => (format!("agent compose: failed to build runtime: {e}"), false),
                                    Ok(new_runtime) => {
                                        self.install_active_runtime(new_runtime);
                                        let turn_result = self.run_turn(&task);
                                        let restore = build_runtime_with_tui_slot(
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
                                        if let Ok(restored) = restore {
                                            self.install_active_runtime(restored);
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
                                // Typed upsert: identify by `(Skill, name)` so reloading
                                // the same skill replaces its body in place, while loading
                                // a different skill stacks alongside.
                                use runtime::{PromptSection, PromptSectionKind, PromptSectionsExt};
                                self.system_prompt.upsert_by_kind(
                                    PromptSection::labeled(PromptSectionKind::Skill, body, name.clone()),
                                );
                                let session = self.active_runtime().session().clone();
                                match build_runtime_with_tui_slot(
                                    session,
                                    self.model.clone(),
                                    self.system_prompt.clone(),
                                    true,
                                    true,
                                    self.allowed_tools.clone(),
                                    self.permission_mode,
                                    None,
                                    self.active_tui_slot(),
                                    self.agent_manager.clone(),
                                ) {
                                    Ok(rt) => {
                                        self.install_active_runtime(rt);
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
                            let last_user = self.active_runtime().session().messages.iter().rev()
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
            SlashCommand::FileCache { action } => {
                (self.run_file_cache_command(action.as_deref()), false)
            }
            SlashCommand::CmdCache { action } => {
                (self.run_cmd_cache_command(action.as_deref()), false)
            }
            SlashCommand::ScrollSpeed { lines } => {
                (run_scroll_speed_command(lines.as_deref()), false)
            }
            SlashCommand::Import { source, dry_run, scope, include_sessions } => {
                // Phase 6.0 foundation: route through the commands-crate handler.
                // Buckets 1–4 will replace this with live pipeline execution.
                let msg = commands::handlers::handle_import_command(
                    source.as_deref(),
                    dry_run,
                    scope.as_deref(),
                    include_sessions,
                );
                (msg, false)
            }
            SlashCommand::Profile { action } => {
                let msg = render_profile_command(action.as_deref());
                (msg, false)
            }
            SlashCommand::Cursor { subcommand } => {
                // Intercept the live Cursor Cloud Agents API calls here.
                // The commands crate handler returns guidance text; this site
                // is where actual CursorClient calls would be triggered for
                // subcommands that need a live response (launch, stream, etc.).
                // For now, dispatch to the guidance handler.
                let msg = commands::handlers::handle_cursor_command(&subcommand);
                (msg, false)
            }
            SlashCommand::HubStatus { package } => {
                // /hub-status <pkg> — delegate to the hub command handler
                // which already supports `status <pkg>` as a sub-action.
                let msg = self.run_hub_command(Some(&format!("status {package}")));
                (msg, false)
            }
            SlashCommand::Unknown(name) => {
                // Intercepted in handle_repl_command_tui. This arm is unreachable.
                (format!("Unknown slash command: /{name}"), false)
            }
            // v2.2.16: Layout is intercepted in handle_repl_command_tui (needs tui ref).
            // This arm is unreachable in TUI mode; in non-TUI mode, return a brief note.
            SlashCommand::Layout { action } => {
                let alias = action.as_deref().unwrap_or("").trim();
                if alias.is_empty() || alias == "list" {
                    let help = "\
/layout list — six variants:\n  \
vertical-split       A: rail + deck\n  \
vertical-split-tabs  D: A + workspace tabs\n  \
three-pane           B: FOCUS/LOG/CONTEXT (vim modal)\n  \
three-pane-tabs      E: B + buffer line\n  \
journal              C: journal + Ctrl-K palette\n  \
journal-tabs         F: C + thread switcher\n\
Requires TUI mode. Use /layout <variant> inside the TUI.";
                    (help.to_string(), false)
                } else {
                    (format!("Layout commands require TUI mode. Start anvil without --non-interactive to use /layout."), false)
                }
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
        let session = self.active_runtime().session().clone();
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
        self.install_active_runtime(runtime);
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
        // v2.2.14 Phase 1 Bucket 1.1: typed prompt sections eliminate the
        // need for the old inline markers (TERSE_MARKER, CUSTOM_STYLE_MARKER).
        // Sections are now identified by kind — OutputStyleCondensed for
        // the built-in terse fragment and OutputStyleCustom for a
        // user-supplied prompt fragment. The body content stays the same so
        // the model sees identical text.
        const TERSE_SKILL_BODY: &str = include_str!("../../commands/bundled/skills/terse/SKILL.md");

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
        // Typed model: identify by kind, not substring. Remove any prior
        // condensed or custom output-style section, then upsert the new one
        // if the chosen style supplies one.
        use runtime::{PromptSection, PromptSectionKind, PromptSectionsExt};
        self.system_prompt.remove_by_kind(&PromptSectionKind::OutputStyleCondensed, None);
        self.system_prompt.remove_by_kind(&PromptSectionKind::OutputStyleCustom, None);

        let is_condensed = matches!(
            new_style,
            OutputStyle::BuiltIn(runtime::BuiltInStyle::Condensed)
        );
        if is_condensed {
            self.system_prompt.upsert_by_kind(
                PromptSection::new(PromptSectionKind::OutputStyleCondensed, TERSE_SKILL_BODY),
            );
        } else if let Some(fragment) = new_style.prompt_fragment() {
            // Custom style: upsert the user-defined fragment under
            // OutputStyleCustom. No marker required — kind disambiguates it.
            self.system_prompt.upsert_by_kind(
                PromptSection::new(PromptSectionKind::OutputStyleCustom, fragment),
            );
        }

        // Rebuild the runtime so the new system prompt takes effect.
        let session = self.active_runtime().session().clone();
        self.install_active_runtime(build_runtime_with_tui_slot(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.active_tui_slot(),
            self.agent_manager.clone(),
        )?);

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
        let current_session = self.active_runtime().session().clone();
        self.install_active_runtime(build_runtime_with_tui_slot(
            current_session,
            self.model.clone(),
            self.system_prompt.clone(),
            !self.chat_mode,
            true,
            new_allowed,
            self.permission_mode,
            None,
            self.active_tui_slot(),
            self.agent_manager.clone(),
        )?);
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
        /// The body text the model sees when fast mode is active.
        const FAST_BODY: &str = "Be concise and direct.";
        self.fast_mode = !self.fast_mode;

        // Typed model: identify by kind (PromptSectionKind::FastMode).
        // No more first()/contains() substring checks or retain() loops.
        use runtime::{PromptSection, PromptSectionKind, PromptSectionsExt};
        if self.fast_mode {
            self.system_prompt
                .upsert_by_kind(PromptSection::new(PromptSectionKind::FastMode, FAST_BODY));
        } else {
            self.system_prompt
                .remove_by_kind(&PromptSectionKind::FastMode, None);
        }

        // Rebuild the runtime so the new system prompt takes effect.
        let session = self.active_runtime().session().clone();
        self.install_active_runtime(build_runtime_with_tui_slot(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.active_tui_slot(),
            self.agent_manager.clone(),
        )?);

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
        let est = self.active_runtime().estimated_tokens();
        lines.push(format!("  - Estimated context: {est} tokens"));

        lines.join("\n")
    }

    // run_search_command, format_search_tool_result, run_failover_command → cmd_provider.rs

    /// Handle a REPL slash command in batch/print mode.
    ///
    /// Phase 5.0.5: This function now delegates to `run_command_for_tui` for
    /// the large set of commands that return a string result, reducing the
    /// per-command duplication that previously lived here.  Commands that
    /// drive LLM turns (Bughunter, Commit, Pr, Issue, Ultraplan, Teleport,
    /// DebugToolCall) and commands with interactive REPL-specific side effects
    /// (Status, Share, Unknown) are still handled inline.
    #[allow(clippy::too_many_lines)]
    fn handle_repl_command(
        &mut self,
        command: SlashCommand,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        // ── LLM-turn commands (cannot go through run_command_for_tui) ────────
        match &command {
            SlashCommand::Bughunter { scope } => {
                self.run_bughunter(scope.as_deref())?;
                return Ok(false);
            }
            SlashCommand::Commit => {
                self.run_commit()?;
                return Ok(true);
            }
            SlashCommand::Pr { context } => {
                self.run_pr(context.as_deref())?;
                return Ok(false);
            }
            SlashCommand::Issue { context } => {
                self.run_issue(context.as_deref())?;
                return Ok(false);
            }
            SlashCommand::Ultraplan { task } => {
                self.run_ultraplan(task.as_deref())?;
                return Ok(false);
            }
            SlashCommand::Teleport { target } => {
                self.run_teleport(target.as_deref())?;
                return Ok(false);
            }
            SlashCommand::DebugToolCall => {
                self.run_debug_tool_call()?;
                return Ok(false);
            }
            // Status has a REPL-specific print_status path (writes structured output
            // to stdout without going through the string-return path).
            SlashCommand::Status => {
                self.print_status();
                return Ok(false);
            }
            // Share has a vault gate that short-circuits before the shared handler.
            SlashCommand::Share { action } => {
                if !runtime::vault_is_session_unlocked() {
                    println!("This command requires the vault to be unlocked. Run /vault unlock first.");
                    return Ok(false);
                }
                let output = self.run_share_command_repl(action.as_deref());
                println!("{output}");
                return Ok(false);
            }
            SlashCommand::Unknown(name) => {
                eprintln!("{}", render_unknown_repl_command(name));
                return Ok(false);
            }
            _ => {}
        }

        // ── All remaining commands: delegate to run_command_for_tui ──────────
        //
        // run_command_for_tui returns (output_string, session_changed).
        // We print the string to stdout (non-empty only) and propagate the
        // session_changed flag.  Errors surface via the `?` operator.
        let (msg, changed) = self.run_command_for_tui(command)?;
        if !msg.is_empty() {
            println!("{msg}");
        }
        Ok(changed)
    }

    // run_goal_command, run_agent_command → cmd_provider.rs

    fn persist_session(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.active_runtime().session().save_to_path(&self.session.path)?;
        // Best-effort: stamp the sidecar with the active model so
        // `anvil --continue` can rebuild the CLI on the right provider.
        // The Session JSON itself has no model field; this lives next to it
        // as `<id>.meta.json`. Failures here are non-fatal — exiting with a
        // saved conversation is more important than the model hint.
        let _ = session_meta::set_session_model(&self.session.id, &self.model);
        Ok(())
    }

    /// Record this session in the daily summary store and print any open items.
    ///
    /// Called once at normal session exit (from both TUI and non-TUI paths).
    /// Failures are swallowed so a write error never prevents Anvil from exiting.
    fn record_daily(&self) {
        use runtime::{DailyStore, SessionSummary, extract_tasks};

        // Acquire the runtime mutex ONCE, derive everything we need, then drop
        // the guard before doing the I/O work. Re-acquiring on the same thread
        // (std::sync::Mutex is non-reentrant) self-deadlocks — this was a real
        // /quit hang in v2.2.12 after Commit 3 wrapped the runtime in
        // Arc<Mutex<...>>.
        let (messages, tokens_used, tool_count) = {
            let guard = self.active_runtime();
            let session_data = guard.session();
            let messages = session_data.messages.clone();
            let tokens_used = guard.usage().cumulative_usage().total_tokens();
            let tool_count = messages.iter().flat_map(|m| &m.blocks).filter(|b| {
                matches!(b, runtime::ContentBlock::ToolUse { .. })
            }).count() as u64;
            (messages, tokens_used, tool_count)
        };

        // Extract tasks from the conversation history.
        let (tasks_completed, tasks_open) = extract_tasks(&messages);

        // Collect modified files from ToolResult outputs that mention a path.
        let files_modified = collect_modified_files(&messages);

        // Count nominations generated this session.
        let nominations_generated = {
            let store = runtime::nominations::NominationStore::new();
            store.list(Some(runtime::nominations::NominationStatus::Pending)).len()
        };

        let duration_ms = self.session_start.elapsed().as_millis() as u64;
        let duration_secs = duration_ms / 1000;
        let messages_count = messages.len();

        // Emit session_end OTel event (no-op when disabled).
        let cost_str = format!("{:.6}", u64::from(tokens_used) as f64 * 0.000_003);
        runtime::otel::session_end(
            &self.session.id,
            duration_ms,
            u64::from(tokens_used),
            &cost_str,
            tool_count,
        );

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

    // print_status → cmd_provider.rs

    /// Atomically swap the active model. Rebuilds the runtime so API
    /// routing follows the new provider on the very next turn, regenerates
    /// the Environment section of the system prompt so the model's
    /// self-identity claim ("Currently loaded model: …") matches reality,
    /// and updates the TUI chrome when a TUI is attached. The session
    /// message history is preserved.
    ///
    /// `new_model` is resolved through [`resolve_model_alias`] before
    /// comparison and storage; the caller does not need to pre-resolve.
    ///
    /// Returns `(previous_model, message_count)`. When the resolved model
    /// equals the current model the call is a no-op: nothing is rebuilt
    /// and `previous_model == self.model`.
    fn apply_model_switch(
        &mut self,
        new_model: &str,
        tui: Option<&mut AnvilTui>,
    ) -> Result<(String, usize), Box<dyn std::error::Error>> {
        use runtime::{PromptSection, PromptSectionKind, PromptSectionsExt, SystemPromptBuilder};

        let previous = self.model.clone();
        let message_count = self.active_runtime().session().messages.len();

        // Check for cross-provider prefix form: "provider_slug/model_id".
        // When present we route to the explicit provider, bypassing
        // detect_provider_kind heuristics (which would mis-classify e.g.
        // "claude-4-sonnet-thinking" as AnvilApi when it's meant for Cursor).
        let (resolved, explicit_kind) = if let Some((slug, bare)) = new_model.split_once('/') {
            let kind = slug_to_provider_kind(slug);
            match kind {
                Some(k) => (resolve_model_alias(bare).to_string(), Some(k)),
                None => {
                    return Err(format!(
                        "unknown provider slug \"{slug}\"; use /model to list providers"
                    ).into());
                }
            }
        } else {
            (resolve_model_alias(new_model).to_string(), None)
        };

        // Same-model (and same provider): no-op, preserve no-rebuild guarantee.
        if resolved == self.model && explicit_kind.is_none() {
            return Ok((previous, message_count));
        }

        // 1. Regenerate ONLY the Environment section. Keep every other
        //    section (Goal, Skill, Memory, ProjectContext, …) byte-
        //    identical so we don't accidentally drop /goal, /skill load,
        //    or other in-session prompt state.
        //
        //    For cross-provider switches the provider label comes from the
        //    explicit kind; for single-provider switches the existing
        //    friendly_provider_label heuristic is used.
        let provider_label: Option<String> = match explicit_kind {
            Some(k) => Some(provider_display_name(k).to_string()),
            None => friendly_provider_label(&resolved),
        };
        let mut builder = SystemPromptBuilder::new().with_model_name(resolved.clone());
        if let Some(ref provider) = provider_label {
            builder = builder.with_provider_name(provider);
        }
        // OS detail matches startup (`build_system_prompt_with_identity`
        // also passes `unknown` for the version). Don't shell out for a
        // real OS version mid-session — it would be inconsistent with
        // the initial prompt build.
        builder = builder.with_os(env::consts::OS, "unknown");
        let new_env_body = builder.render_environment_section();
        self.system_prompt
            .upsert_by_kind(PromptSection::new(PromptSectionKind::Environment, new_env_body));

        // 2. Rebuild the runtime so the next turn talks to the new
        //    provider through the right ApiClient. Without this, the
        //    chrome swaps but inference still hits the previous backend.
        let session = self.active_runtime().session().clone();
        if let Some(kind) = explicit_kind {
            // Cross-provider switch: use explicit kind to bypass detect_provider_kind.
            self.install_active_runtime(build_runtime_for_provider(
                session,
                resolved.clone(),
                kind,
                self.system_prompt.clone(),
                true,
                true,
                self.allowed_tools.clone(),
                self.permission_mode,
                None,
                self.active_tui_slot(),
                self.agent_manager.clone(),
            )?);
        } else {
            self.install_active_runtime(build_runtime_with_tui_slot(
                session,
                resolved.clone(),
                self.system_prompt.clone(),
                true,
                true,
                self.allowed_tools.clone(),
                self.permission_mode,
                None,
                self.active_tui_slot(),
                self.agent_manager.clone(),
            )?);
        }

        // 3. Commit the model state and update TUI chrome (status bar,
        //    Thinking header, tab title).  The stored model is always the
        //    bare ID so downstream code (MessageRequest, format_model_report)
        //    never sees the provider prefix.
        self.model = resolved;
        if let Some(tui) = tui {
            tui.set_model(self.model.clone());
        }
        Ok((previous, message_count))
    }

    fn set_model(&mut self, model: Option<String>) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(model) = model else {
            println!(
                "{}",
                format_model_report(
                    &self.model,
                    self.active_runtime().session().messages.len(),
                    self.active_runtime().usage().turns(),
                )
            );
            return Ok(false);
        };

        let resolved = resolve_model_alias(&model).to_string();

        if resolved == self.model {
            println!(
                "{}",
                format_model_report(
                    &self.model,
                    self.active_runtime().session().messages.len(),
                    self.active_runtime().usage().turns(),
                )
            );
            return Ok(false);
        }

        let (previous, message_count) = self.apply_model_switch(&resolved, None)?;
        println!(
            "{}",
            format_model_switch_report(&previous, &self.model, message_count)
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
        let session = self.active_runtime().session().clone();
        self.permission_mode = permission_mode_from_label(normalized);
        self.install_active_runtime(build_runtime_with_tui_slot(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.active_tui_slot(),
            self.agent_manager.clone(),
        )?);
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
        self.install_active_runtime(build_runtime_with_tui_slot(
            Session::new(),
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.active_tui_slot(),
            self.agent_manager.clone(),
        )?);
        println!(
            "Session cleared\n  Mode             fresh session\n  Preserved model  {}\n  Permission mode  {}\n  Session          {}",
            self.model,
            self.permission_mode.as_str(),
            self.session.id,
        );
        Ok(true)
    }

    fn print_cost(&self) {
        let cumulative = self.active_runtime().usage().cumulative_usage();
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
        self.install_active_runtime(build_runtime_with_tui_slot(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.active_tui_slot(),
            self.agent_manager.clone(),
        )?);
        self.session = handle;
        println!(
            "{}",
            format_resume_report(
                &self.session.path.display().to_string(),
                message_count,
                self.active_runtime().usage().turns(),
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
        self.install_active_runtime(build_runtime_with_tui_slot(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.active_tui_slot(),
            self.agent_manager.clone(),
        )?);
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
        // `anvil agents live` / `anvil agents monitor` — list live subagents
        // across every running Anvil process on this machine (CC-139-F1, #462).
        // Any other arg falls through to the static agent-definition listing.
        if let Some(arg) = args.map(str::trim)
            && matches!(arg, "live" | "monitor")
        {
            println!("{}", render_live_agents_listing());
            return Ok(());
        }
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
    ///
    /// v2.2.14: identify Skill sections by their typed `(kind, label)` pair.
    /// The label carries the canonical skill name set by `/skill load`, so we
    /// no longer have to scan section bodies for a `# skill:<name> —` marker.
    fn loaded_skills_snapshot(&self) -> Vec<commands::agents::SkillSummary> {
        use commands::agents::{discover_skill_roots, load_skills_from_roots};
        let cwd = std::env::current_dir().unwrap_or_default();
        let roots = discover_skill_roots(&cwd);
        let all_skills = load_skills_from_roots(&roots).unwrap_or_default();
        let mut loaded = Vec::new();
        for prompt_part in &self.system_prompt {
            if prompt_part.kind != runtime::PromptSectionKind::Skill {
                continue;
            }
            let Some(label) = prompt_part.label.as_deref() else {
                continue;
            };
            for skill in &all_skills {
                if skill.name == label {
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
        let export_path = resolve_export_path(requested_path, self.active_runtime().session())?;
        let content = if format == Some("md") {
            render_export_markdown(self.active_runtime().session())
        } else {
            render_export_text(self.active_runtime().session())
        };
        fs::write(&export_path, content)?;
        let fmt_label = if format == Some("md") { "markdown" } else { "text" };
        println!(
            "Export\n  Result           wrote {fmt_label} transcript\n  File             {}\n  Messages         {}",
            export_path.display(),
            self.active_runtime().session().messages.len(),
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
                self.install_active_runtime(build_runtime_with_tui_slot(
                    session,
                    self.model.clone(),
                    self.system_prompt.clone(),
                    true,
                    true,
                    self.allowed_tools.clone(),
                    self.permission_mode,
                    None,
                    self.active_tui_slot(),
                    self.agent_manager.clone(),
                )?);
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
                self.install_active_runtime(build_runtime_with_tui_slot(
                    session,
                    self.model.clone(),
                    self.system_prompt.clone(),
                    true,
                    true,
                    self.allowed_tools.clone(),
                    self.permission_mode,
                    None,
                    self.active_tui_slot(),
                    self.agent_manager.clone(),
                )?);
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
        self.install_active_runtime(build_runtime_with_tui_slot(
            session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.active_tui_slot(),
            self.agent_manager.clone(),
        )?);
        self.session = handle;
        Ok((format_resume_report(
            &self.session.path.display().to_string(),
            message_count,
            self.active_runtime().usage().turns(),
        ), true))
    }

    fn reload_runtime_features(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let current_session = self.active_runtime().session().clone();
        self.install_active_runtime(build_runtime_with_tui_slot(
            current_session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.active_tui_slot(),
            self.agent_manager.clone(),
        )?);
        self.persist_session()
    }

    fn compact(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Archive the full conversation before discarding messages.
        let _ = self.history_archiver.archive_session(
            &self.session.id,
            self.active_runtime().session(),
            &self.model,
            "Manual /compact",
        );

        let result = self.active_runtime_mut().compact(CompactionConfig::default());
        let removed = result.removed_message_count;
        let kept = result.compacted_session.messages.len();
        let skipped = removed == 0;
        self.install_active_runtime(build_runtime_with_tui_slot(
            result.compacted_session,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
            self.active_tui_slot(),
            self.agent_manager.clone(),
        )?);
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
        let estimated = self.active_runtime().estimated_tokens();
        let context_max = max_tokens_for_model(&self.model) as usize;
        let threshold_pct = HistoryArchiver::compact_threshold_pct() as usize;
        let threshold = context_max * threshold_pct / 100;

        if estimated < threshold {
            return None;
        }

        // Archive before discarding messages.
        let archive_result = self.history_archiver.archive_session(
            &self.session.id,
            self.active_runtime().session(),
            &self.model,
            &format!("Auto-compacted: estimated {estimated} tokens exceeded {threshold_pct}% of {context_max} context limit"),
        );

        let result = self.active_runtime_mut().compact(CompactionConfig::default());
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
            self.active_tui_slot(),
            self.agent_manager.clone(),
        )
        .map(|new_runtime| {
            self.install_active_runtime(new_runtime);
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
        let session = self.active_runtime().session().clone();
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

    // run_share_command_repl, run_hub_command → cmd_provider.rs

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

    // ─── T5-Ssh-F: /ssh TUI dispatch ─────────────────────────────────────────

    /// Handle the `/ssh` slash command in TUI mode.
    // ─── /layout command (v2.2.16) ───────────────────────────────────────────

    /// Handle the `/layout` slash command.
    ///
    /// Grammar (per spec §3):
    ///   /layout                       → prints current layout + list hint
    ///   /layout list                  → list all 6 variants with descriptions
    ///   /layout <kind> [--tabs|--no-tabs]  → switch live
    ///   /layout reset                 → restore default (vertical-split-tabs)
    ///
    /// Emits an OTel event `layout_switched { from_alias, to_alias }` on change.
    fn handle_layout_command(
        &mut self,
        action: Option<&str>,
        tui: &mut crate::tui::AnvilTui,
    ) -> String {
        use runtime::{TuiLayoutConfig, TuiLayoutKind, tui_layout_kind_from_alias, tui_layout_to_alias};

        let current = tui.tui_layout;
        let current_alias = tui_layout_to_alias(&current);

        let action = action.unwrap_or("").trim();

        if action.is_empty() {
            // Bare /layout — show current + hint.
            return format!(
                "Current layout: {} ({}).\n\
                 Use /layout list for all variants, or /layout <kind> to switch.",
                current_alias,
                if current.tabs { "tabs on" } else { "tabs off" }
            );
        }

        if action == "list" {
            return "\
/layout variants:\n  \
vertical-split       Layout A: rail + swappable right deck (tabs: off)\n  \
vertical-split-tabs  Layout D: A + workspace tabs\n  \
three-pane           Layout B: FOCUS/LOG/CONTEXT, vim modal (tabs: off)\n  \
three-pane-tabs      Layout E: B + vim buffer line\n  \
journal              Layout C: timestamped journal, Ctrl-K palette (tabs: off)\n  \
journal-tabs         Layout F: C + thread switcher\n\n\
Usage: /layout <variant>   /layout <kind> --tabs   /layout <kind> --no-tabs"
                .to_string();
        }

        if action == "reset" {
            let default = TuiLayoutConfig::default();
            let to_alias = tui_layout_to_alias(&default);
            tui.set_layout(default);
            return format!("Layout reset to {to_alias} (vertical-split + tabs).");
        }

        // Parse `<kind> [--tabs | --no-tabs]`.
        let (kind_part, flag_part) = if let Some((k, f)) = action.split_once(' ') {
            (k.trim(), f.trim())
        } else {
            (action, "")
        };

        // Determine tabs override from flag.
        let tabs_override: Option<bool> = if flag_part.contains("--no-tabs") {
            Some(false)
        } else if flag_part.contains("--tabs") {
            Some(true)
        } else {
            None
        };

        // Resolve the kind. Try with `-tabs` suffix appended (alias table includes it).
        let resolved = tui_layout_kind_from_alias(kind_part)
            .or_else(|| tui_layout_kind_from_alias(&format!("{kind_part}-tabs")));

        let Some(mut new_cfg) = resolved else {
            return format!(
                "Unknown layout: {kind_part:?}. Use /layout list for all variants."
            );
        };

        // Apply tabs flag if specified, overriding alias-embedded default.
        if let Some(tabs) = tabs_override {
            new_cfg.tabs = tabs;
        } else if tabs_override.is_none() && !kind_part.ends_with("-tabs") {
            // No explicit flag, no -tabs suffix → keep current tabs setting.
            new_cfg.tabs = current.tabs;
        }

        let to_alias = tui_layout_to_alias(&new_cfg);

        if new_cfg == current {
            return format!("Already on layout {to_alias}. No change.");
        }

        tui.set_layout(new_cfg);

        format!(
            "Layout switched to {} ({}).",
            to_alias,
            if new_cfg.tabs { "tabs on" } else { "tabs off" }
        )
    }

    ///
    /// - bare `/ssh`                → open the SSH form modal
    /// - `/ssh <alias>`             → load alias from vault and prefill form
    /// - `/ssh save <alias>`        → save active SSH tab's config to vault
    /// - `/ssh list`                → list saved aliases
    fn handle_ssh_tui_command(
        &mut self,
        args: Option<&str>,
        tui: &mut crate::tui::AnvilTui,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let action = args.unwrap_or("").trim();

        // `/ssh save <alias>` — persist the active tab's SSH config.
        if let Some(alias) = action.strip_prefix("save ").map(str::trim) {
            if alias.is_empty() {
                tui.push_system("/ssh save requires an alias name".to_string());
                return Ok(());
            }
            if !runtime::vault_is_session_unlocked() {
                tui.push_system(
                    "/ssh save requires the vault to be unlocked. Run /vault unlock first."
                        .to_string(),
                );
                return Ok(());
            }
            // Retrieve config from the active SSH tab.
            let config_opt = tui
                .active_tab()
                .ssh
                .as_ref()
                .map(|s| runtime::ssh::SshConfig {
                    host: {
                        // destination is "user@host:port"; parse host out.
                        let dest = &s.destination;
                        let after_at = dest.split('@').nth(1).unwrap_or(dest);
                        // rsplit(':') returns port first, then host
                        let host_part = after_at.rsplit(':').nth(1).unwrap_or(after_at);
                        host_part.to_string()
                    },
                    port: {
                        let dest = &s.destination;
                        let after_at = dest.split('@').nth(1).unwrap_or(dest);
                        after_at
                            .rsplit(':')
                            .next()
                            .and_then(|p| p.parse().ok())
                            .unwrap_or(22)
                    },
                    user: {
                        let dest = &s.destination;
                        dest.split('@').next().unwrap_or("").to_string()
                    },
                    // We can't reconstruct the auth credential from the live tab
                    // (it was consumed at connect-time). Default to Agent so the
                    // saved alias at least has the host/port/user.
                    auth: runtime::ssh::SshAuthMethod::Agent,
                });
            match config_opt {
                None => {
                    tui.push_system(
                        "No active SSH tab. Connect first with /ssh".to_string(),
                    );
                }
                Some(config) => {
                    let alias_str = alias.to_string();
                    let result = runtime::with_session_vault(|vm| {
                        runtime::ssh::save_ssh_alias(vm, &alias_str, &config)
                            .map_err(|e| runtime::vault::VaultError::Serialization(e.to_string()))
                    });
                    match result {
                        Ok(()) => tui.push_system(format!(
                            "SSH alias '{alias}' saved (auth: agent)."
                        )),
                        Err(e) => tui.push_system(format!(
                            "Failed to save alias '{alias}': {e}"
                        )),
                    }
                }
            }
            return Ok(());
        }

        // `/ssh list` — enumerate saved aliases.
        if action == "list" {
            if !runtime::vault_is_session_unlocked() {
                tui.push_system(
                    "/ssh list requires the vault to be unlocked.".to_string(),
                );
                return Ok(());
            }
            let result = runtime::with_session_vault(|vm| {
                runtime::ssh::list_ssh_aliases(vm)
                    .map_err(|e| runtime::vault::VaultError::Serialization(e.to_string()))
            });
            match result {
                Ok(aliases) if aliases.is_empty() => {
                    tui.push_system(
                        "No SSH aliases saved. Use /ssh save <alias> to create one."
                            .to_string(),
                    );
                }
                Ok(aliases) => {
                    let mut msg = "Saved SSH aliases:\n".to_string();
                    for a in &aliases {
                        let _ = std::fmt::Write::write_fmt(
                            &mut msg,
                            format_args!("  {a}\n"),
                        );
                    }
                    tui.push_system(msg);
                }
                Err(e) => tui.push_system(format!("Vault error: {e}")),
            }
            return Ok(());
        }

        // `/ssh <alias>` — load alias and prefill form.
        if !action.is_empty() {
            // Try to load from vault; fall back to treating arg as host spec.
            let config_and_alias: Option<(runtime::ssh::SshConfig, Option<String>)> =
                if runtime::vault_is_session_unlocked() {
                    let alias_str = action.to_string();
                    runtime::with_session_vault(|vm| {
                        runtime::ssh::load_ssh_alias(vm, &alias_str)
                            .map_err(|e| runtime::vault::VaultError::Serialization(e.to_string()))
                    })
                    .ok()
                    .map(|cfg| (cfg, Some(action.to_string())))
                } else {
                    None
                };

            use crate::tui::ssh_form::SshFormState;
            let mut form = SshFormState::new();
            if let Some((cfg, alias)) = config_and_alias {
                form.prefill(&cfg, alias.as_deref());
            } else {
                // Treat the bare argument as a host spec (user@host:port).
                let arg = action;
                if let Some((user, rest)) = arg.split_once('@') {
                    form.user = user.to_string();
                    if let Some((host, port_str)) = rest.rsplit_once(':') {
                        form.host = host.to_string();
                        form.port_str = port_str.to_string();
                    } else {
                        form.host = rest.to_string();
                    }
                } else {
                    form.host = arg.to_string();
                }
            }
            tui.ssh_form = Some(form);
            return Ok(());
        }

        // Bare `/ssh` — open blank form.
        tui.ssh_form = Some(crate::tui::ssh_form::SshFormState::new());
        Ok(())
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
        println!("{}", render_last_tool_debug_report(self.active_runtime().session())?);
        Ok(())
    }
}

// ─── CC-139-F1 — cross-session live agent view (#462) ─────────────────────────

/// Render the output of `anvil agents live` / `anvil agents monitor`.
///
/// Reads every live snapshot under `~/.anvil/agents/`, filters dead PIDs, and
/// renders a one-line-per-agent table.  Empty state is announced explicitly.
fn render_live_agents_listing() -> String {
    let snapshots = runtime::agent_snapshot::read_all_snapshots();
    if snapshots.is_empty() {
        return "No live anvil agents.".to_string();
    }

    let total_agents: usize = snapshots.iter().map(|s| s.agents.len()).sum();
    let mut out = format!(
        "Live anvil agents ({} agent{} across {} process{}):\n",
        total_agents,
        if total_agents == 1 { "" } else { "s" },
        snapshots.len(),
        if snapshots.len() == 1 { "" } else { "es" },
    );

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    for snap in &snapshots {
        if snap.agents.is_empty() {
            out.push_str(&format!(
                "  PID {pid:<6}  {sess:<width$}  (no active agents)\n",
                pid = snap.pid,
                sess = truncate_session_id(&snap.session_id, SHORT_SESSION_ID_WIDTH),
                width = SHORT_SESSION_ID_WIDTH,
            ));
            continue;
        }
        for agent in &snap.agents {
            let elapsed = now.saturating_sub(agent.started_at);
            out.push_str(&format!(
                "  PID {pid:<6}  {sess:<width$}  task-{id:<4}  {kind:<14}  {status:<10}  {elapsed}\n",
                pid = snap.pid,
                sess = truncate_session_id(&snap.session_id, SHORT_SESSION_ID_WIDTH),
                id = agent.id,
                kind = truncate_session_id(&agent.kind, 14),
                status = agent.status,
                elapsed = format_elapsed(elapsed),
                width = SHORT_SESSION_ID_WIDTH,
            ));
        }
    }
    out
}

// CC-DRIFT-F1: short session ID width in `anvil agents` listings. 12 chars
// balances CC's ~8-char convention against Anvil's timestamp-shaped session
// ids that need a few more characters to stay distinct in lists.
const SHORT_SESSION_ID_WIDTH: usize = 12;

/// Truncate a session id (or any short label) to `max` chars, replacing the
/// tail with `…` when overlong.  Pure ASCII width is assumed — these are
/// timestamp-based session ids, not user-supplied free text.
fn truncate_session_id(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod cc_139_f1_tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Mutex;

    // Snapshot tests mutate ANVIL_CONFIG_HOME — serialise them across this module
    // AND cross-module via `serial(anvil_config_home)` to avoid races with
    // uninstall::tests which also mutates the same env var.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        prev: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn new(home: &std::path::Path) -> Self {
            let prev = std::env::var_os("ANVIL_CONFIG_HOME");
            // SAFETY: tests are serialised by ENV_LOCK; we are the sole writer.
            unsafe { std::env::set_var("ANVIL_CONFIG_HOME", home); }
            Self { prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: tests are serialised by ENV_LOCK; we are the sole writer.
            unsafe {
                match self.prev.take() {
                    Some(value) => std::env::set_var("ANVIL_CONFIG_HOME", value),
                    None => std::env::remove_var("ANVIL_CONFIG_HOME"),
                }
            }
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn empty_state_message() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = EnvGuard::new(tmp.path());

        let out = render_live_agents_listing();
        assert!(out.contains("No live anvil agents"), "expected empty-state line, got: {out:?}");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn lists_live_snapshot_entries() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = EnvGuard::new(tmp.path());

        let pid = std::process::id();
        let entries = vec![runtime::agent_snapshot::AgentEntry {
            id: "1".to_string(),
            name: "scout".to_string(),
            kind: "explore".to_string(),
            status: "running".to_string(),
            started_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }];
        runtime::agent_snapshot::write_snapshot(pid, "session-xyz", &entries);

        let out = render_live_agents_listing();
        assert!(out.contains("PID"), "header expected: {out:?}");
        assert!(out.contains("session-xyz"), "session id expected: {out:?}");
        assert!(out.contains("explore"), "kind label expected: {out:?}");
        assert!(out.contains("running"), "status expected: {out:?}");

        runtime::agent_snapshot::clear_snapshot(pid);
    }

    #[test]
    fn format_elapsed_renders_compactly() {
        assert_eq!(format_elapsed(5), "5s");
        assert_eq!(format_elapsed(75), "1m15s");
        assert_eq!(format_elapsed(3725), "1h02m");
    }

    // CC-DRIFT-F1: long session IDs in `anvil agents` listings must be
    // truncated to the short width so each row stays scannable.
    #[test]
    #[serial(anvil_config_home)]
    fn live_listing_truncates_long_session_ids() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = EnvGuard::new(tmp.path());

        let pid = std::process::id();
        let long_id = "20260512T123456-abcdef-very-long-session";
        let entries = vec![runtime::agent_snapshot::AgentEntry {
            id: "1".to_string(),
            name: "scout".to_string(),
            kind: "explore".to_string(),
            status: "running".to_string(),
            started_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }];
        runtime::agent_snapshot::write_snapshot(pid, long_id, &entries);

        let out = render_live_agents_listing();
        runtime::agent_snapshot::clear_snapshot(pid);

        assert!(
            !out.contains(long_id),
            "full session id must not appear when over short width: {out:?}"
        );
        let truncated = truncate_session_id(long_id, SHORT_SESSION_ID_WIDTH);
        assert_eq!(truncated.chars().count(), SHORT_SESSION_ID_WIDTH);
        assert!(
            out.contains(&truncated),
            "truncated session id `{truncated}` must appear in output: {out:?}"
        );
    }
}

#[cfg(test)]
mod tests;
