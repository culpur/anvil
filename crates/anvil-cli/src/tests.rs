use super::{
    filter_tool_specs, format_compact_report, format_cost_report,
    format_model_report, format_model_switch_report,
    format_permissions_report, format_permissions_switch_report, format_resume_report,
    format_status_report,
    normalize_permission_mode, parse_args, parse_git_status_metadata,
    render_config_report, render_memory_report,
    render_repl_help, render_unknown_repl_command, resolve_model_alias,
    slash_command_completion_candidates, status_context,
    CliAction, CliOutputFormat,
    SlashCommand, StatusUsage,
};
use super::providers::{
    describe_tool_progress, permission_policy,
    InternalPromptProgressEvent, InternalPromptProgressState,
    format_internal_prompt_progress_line,
};
use super::format_tool::{push_output_block, response_to_events};
use super::help::print_help_to;
use commands::resume_supported_slash_commands;
use crate::format_tool::{format_tool_call_start, format_tool_result, tool_result_summary};
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
    let action = parse_args(&[]).expect("args should parse");
    match action {
        CliAction::Repl { model, allowed_tools, permission_mode } => {
            assert!(!model.is_empty(), "model should not be empty");
            assert!(allowed_tools.is_none());
            assert_eq!(permission_mode, PermissionMode::DangerFullAccess);
        }
        other => panic!("Expected Repl, got {other:?}"),
    }
}

#[test]
fn parses_prompt_subcommand() {
    let args = vec![
        "prompt".to_string(),
        "hello".to_string(),
        "world".to_string(),
    ];
    let action = parse_args(&args).expect("args should parse");
    match action {
        CliAction::Prompt { prompt, model, output_format, allowed_tools, permission_mode } => {
            assert_eq!(prompt, "hello world");
            assert!(!model.is_empty(), "model should not be empty");
            assert_eq!(output_format, CliOutputFormat::Text);
            assert!(allowed_tools.is_none());
            assert_eq!(permission_mode, PermissionMode::DangerFullAccess);
        }
        other => panic!("Expected Prompt, got {other:?}"),
    }
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
    let action = parse_args(&args).expect("args should parse");
    match action {
        CliAction::Repl { model, allowed_tools, permission_mode } => {
            assert!(!model.is_empty());
            assert!(allowed_tools.is_none());
            assert_eq!(permission_mode, PermissionMode::ReadOnly);
        }
        other => panic!("Expected Repl, got {other:?}"),
    }
}

#[test]
fn parses_allowed_tools_flags_with_aliases_and_lists() {
    let args = vec![
        "--allowedTools".to_string(),
        "read,glob".to_string(),
        "--allowed-tools=write_file".to_string(),
    ];
    let action = parse_args(&args).expect("args should parse");
    match action {
        CliAction::Repl { model, allowed_tools, permission_mode } => {
            assert!(!model.is_empty());
            let tools = allowed_tools.expect("allowed_tools should be Some");
            assert!(tools.contains("read_file"));
            assert!(tools.contains("glob_search"));
            assert!(tools.contains("write_file"));
            assert_eq!(permission_mode, PermissionMode::DangerFullAccess);
        }
        other => panic!("Expected Repl, got {other:?}"),
    }
}

#[test]
fn rejects_unknown_allowed_tools() {
    let error = parse_args(&["--allowedTools".to_string(), "teleport".to_string()])
        .expect_err("tool should be rejected");
    assert!(error.contains("unsupported tool in --allowedTools: teleport"));
}

// FEAT-42 -----------------------------------------------------------------
// CLI-level smoke tests for --plugin-dir / --plugin-url.  Heavy lifting
// (zip extraction, traversal rejection, https-only enforcement) is
// covered in `plugins::session_plugins::tests`; here we just verify
// parse_args wires the flags to the right surface and surfaces a useful
// error when validation fails up front.

#[test]
fn rejects_plugin_url_with_http_scheme() {
    let error = parse_args(&[
        "--plugin-url".to_string(),
        "http://example.com/plugin.zip".to_string(),
    ])
    .expect_err("http:// must be rejected before any network I/O");
    assert!(
        error.contains("HTTPS") || error.contains("https"),
        "got: {error}"
    );
}

#[test]
fn rejects_plugin_sha256_without_url() {
    let error = parse_args(&[
        "--plugin-sha256".to_string(),
        "0".repeat(64),
    ])
    .expect_err("--plugin-sha256 without a URL must be rejected");
    assert!(error.contains("--plugin-sha256"));
    assert!(error.contains("--plugin-url"));
}

#[test]
fn rejects_plugin_dir_pointing_at_nonexistent_path() {
    let error = parse_args(&[
        "--plugin-dir".to_string(),
        "/definitely/does/not/exist/anvil-feat42".to_string(),
    ])
    .expect_err("--plugin-dir on a missing path must be rejected");
    assert!(error.contains("--plugin-dir"));
}

#[test]
fn plugin_dir_passthrough_directory_yields_repl_action() {
    // Use the workspace root — we know it exists.
    let workspace_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|crates| crates.parent())
            .expect("workspace root")
            .to_path_buf();
    let action = parse_args(&[
        "--plugin-dir".to_string(),
        workspace_root.to_string_lossy().into_owned(),
    ])
    .expect("--plugin-dir on a directory should parse cleanly");
    assert!(matches!(action, CliAction::Repl { .. }));
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
fn parses_chain_list_via_anvil_subcommand() {
    use commands::ChainSubcommand;
    assert_eq!(
        parse_args(&["chain".to_string()]).expect("chain should parse"),
        CliAction::Chain { subcommand: ChainSubcommand::List }
    );
    assert_eq!(
        parse_args(&["chain".to_string(), "list".to_string()]).expect("chain list should parse"),
        CliAction::Chain { subcommand: ChainSubcommand::List }
    );
}

#[test]
fn parses_chain_install_with_slug() {
    use commands::ChainSubcommand;
    assert_eq!(
        parse_args(&[
            "chain".to_string(),
            "install".to_string(),
            "my-chain".to_string()
        ])
        .expect("chain install should parse"),
        CliAction::Chain {
            subcommand: ChainSubcommand::Install { slug: "my-chain".to_string() }
        }
    );
    let err = parse_args(&["chain".to_string(), "install".to_string()])
        .expect_err("missing slug must surface a friendly error");
    assert!(err.contains("missing <slug>"), "{err}");
}

#[test]
fn parses_chain_run_with_target_and_extra_args() {
    use commands::ChainSubcommand;
    assert_eq!(
        parse_args(&[
            "chain".to_string(),
            "run".to_string(),
            "path/to/chain.yaml".to_string()
        ])
        .expect("chain run should parse"),
        CliAction::Chain {
            subcommand: ChainSubcommand::Run {
                target: "path/to/chain.yaml".to_string(),
                args: Vec::new(),
            }
        }
    );
    assert_eq!(
        parse_args(&[
            "chain".to_string(),
            "run".to_string(),
            "slug".to_string(),
            "extra1".to_string(),
            "extra2".to_string()
        ])
        .expect("chain run with args should parse"),
        CliAction::Chain {
            subcommand: ChainSubcommand::Run {
                target: "slug".to_string(),
                args: vec!["extra1".to_string(), "extra2".to_string()],
            }
        }
    );
}

#[test]
fn parses_direct_chain_slash_command() {
    use commands::ChainSubcommand;
    assert_eq!(
        parse_args(&["/chain".to_string()]).expect("/chain should parse"),
        CliAction::Chain { subcommand: ChainSubcommand::List }
    );
    assert_eq!(
        parse_args(&[
            "/chain".to_string(),
            "run".to_string(),
            "some/path/chain.yaml".to_string()
        ])
        .expect("/chain run should parse"),
        CliAction::Chain {
            subcommand: ChainSubcommand::Run {
                target: "some/path/chain.yaml".to_string(),
                args: Vec::new(),
            }
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
    assert!(help.contains("/config [env|hooks|model|plugins|tui_mouse_capture] [on|off]"));
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
    // v2.2.6: productivity, knowledge, daily added as resume-supported
    // v2.2.16: layout added as resume-supported
    // v2.2.17 task #557: rewind added as resume-supported (CC parity)
    assert_eq!(
        names,
        vec![
            "help", "status", "compact", "rewind", "clear", "cost", "config", "memory", "init",
            "diff", "version", "export", "agents", "skills", "qmd", "history", "doctor", "tokens",
            "history-archive", "configure", "language", "sleep", "productivity", "knowledge",
            "daily", "layout",
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
fn model_switch_report_warns_on_mid_conversation_switch() {
    // Claude Code v2.1.108 parity: mid-conversation switches re-read the
    // whole history uncached against the new model, which is a real cost
    // surprise. Surface the warning when message_count > 0.
    let mid = format_model_switch_report("sonnet", "opus", 42);
    assert!(
        mid.contains("Warning") && mid.contains("uncached"),
        "mid-conversation switch must warn: {mid}"
    );
}

#[test]
fn model_switch_report_no_warning_on_fresh_session() {
    // Fresh session (message_count == 0) — no warning, nothing to re-read.
    let fresh = format_model_switch_report("sonnet", "opus", 0);
    assert!(
        !fresh.contains("Warning"),
        "fresh-session switch should not warn: {fresh}"
    );
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
        Some(SlashCommand::Clear { confirm: false, all_tabs: false })
    );
    assert_eq!(
        SlashCommand::parse("/clear --confirm"),
        Some(SlashCommand::Clear { confirm: true, all_tabs: false })
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
        Some(SlashCommand::Clear { confirm: true, all_tabs: false })
    );
    assert_eq!(
        SlashCommand::parse("/config"),
        Some(SlashCommand::Config { section: None, value: None })
    );
    assert_eq!(
        SlashCommand::parse("/config env"),
        Some(SlashCommand::Config {
            section: Some("env".to_string()),
            value: None,
        })
    );
    assert_eq!(
        SlashCommand::parse("/memory"),
        Some(SlashCommand::Memory { action: None })
    );
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

    let converted = super::providers::convert_messages(&messages);
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
fn tool_result_summary_extracts_bash_stdout_first_line() {
    // Bug from screenshot: bash result was rendering as `bash [ok]: {` because
    // the JSON output's first line was just the opening brace.
    let json = r#"{"stdout":"hello world\nsecond line","stderr":"","interrupted":false}"#;
    let summary = tool_result_summary("bash", json, false);
    assert!(
        summary.contains("hello world"),
        "expected stdout in summary, got: {summary}"
    );
    assert!(
        !summary.starts_with('{'),
        "summary must not start with literal JSON brace, got: {summary}"
    );
    assert!(
        summary.contains("+1 more line"),
        "should signal multi-line, got: {summary}"
    );
}

#[test]
fn tool_result_summary_falls_back_to_known_keys_for_generic_tools() {
    // TeamCreate / TeamAddMember / MCP tools return free-form JSON. The
    // generic fallback should pull out a meaningful field rather than `{`.
    let json = r#"{"id":"team-42","name":"backend-fleet","members":3}"#;
    let summary = tool_result_summary("TeamCreate", json, false);
    assert!(
        summary.contains("backend-fleet") || summary.contains("team-42"),
        "expected name or id in summary, got: {summary}"
    );
    assert!(
        !summary.trim().starts_with('{'),
        "summary must not be raw JSON brace, got: {summary}"
    );
}

#[test]
fn tool_result_summary_lists_keys_for_unknown_shape() {
    let json = r#"{"foo":42,"bar":[1,2,3],"baz":{"nested":true}}"#;
    let summary = tool_result_summary("MystyTool", json, false);
    assert!(
        summary.starts_with("keys:"),
        "expected key listing fallback, got: {summary}"
    );
    assert!(summary.contains("foo"));
    assert!(summary.contains("bar"));
}

#[test]
fn tool_result_summary_handles_empty_bash_output() {
    let json = r#"{"stdout":"","stderr":"","interrupted":false}"#;
    let summary = tool_result_summary("bash", json, false);
    assert_eq!(summary, "(no output)");
}

#[test]
fn tool_result_summary_passes_through_error_text() {
    let summary = tool_result_summary("anything", "EACCES: permission denied", true);
    assert!(summary.contains("permission denied"));
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

// ─── Task #626 — clippy gate regression test ──────────────────────────────────

/// Every TUI-touching file in this crate must carry the
/// `#![deny(clippy::print_stdout, clippy::print_stderr)]` attribute so a
/// new `println!` / `eprintln!` regression fails clippy at PR time
/// instead of corrupting the alt-screen at runtime.
///
/// This test reads the source files at test-run time and asserts the
/// gate is in place.  If a file is intentionally moved off the deny
/// list, drop its entry from `EXPECTED` here and document why in
/// `audit/println-tui-reachability-2026-05-18.md`.
#[test]
fn clippy_print_gate_is_in_place_on_tui_touching_files() {
    let expected = [
        // anvil-cli crate
        "crates/anvil-cli/src/cmd_provider.rs",
        "crates/anvil-cli/src/cmd_ai.rs",
        "crates/anvil-cli/src/cmd_static.rs",
        "crates/anvil-cli/src/providers.rs",
        "crates/anvil-cli/src/remote_control.rs",
        "crates/anvil-cli/src/tui/mod.rs",
        "crates/anvil-cli/src/utils.rs",
        "crates/anvil-cli/src/vault.rs",
        // commands crate
        "crates/commands/src/handlers.rs",
        "crates/commands/src/skill_chaining.rs",
    ];
    // Walk up from `crates/anvil-cli` to the repo root.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest
        .parent()
        .and_then(std::path::Path::parent)
        .expect("repo root above crates/anvil-cli");

    let needle = "#![deny(clippy::print_stdout, clippy::print_stderr)]";
    for rel in expected {
        let path = repo_root.join(rel);
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {rel}: {e}"));
        assert!(
            body.contains(needle),
            "task #626: {rel} is missing the print-stdout/stderr deny gate.  \
             Add `{needle}` at the top of the file or remove it from the \
             EXPECTED list and document why in the audit file."
        );
    }
}

// ─── Task #627 — modal wiring regression tests ────────────────────────────────

#[test]
fn vault_unlock_needs_modal_bare_unlock_is_true() {
    assert!(super::vault_unlock_needs_modal("unlock"));
}

#[test]
fn vault_unlock_needs_modal_with_inline_pw_is_false() {
    // `/vault unlock <pw>` carries the password inline — no modal.
    assert!(!super::vault_unlock_needs_modal("unlock hunter2"));
}

#[test]
fn vault_unlock_needs_modal_other_subcommands_false() {
    for s in &[
        "", "status", "setup", "lock", "store myvault",
        "get foo", "list", "delete foo", "totp list",
    ] {
        assert!(
            !super::vault_unlock_needs_modal(s),
            "{s:?} should not open the password modal"
        );
    }
}

#[test]
fn vault_unlock_needs_modal_handles_whitespace() {
    // Leading / trailing whitespace must not cause us to drop the modal.
    assert!(super::vault_unlock_needs_modal("  unlock  "));
}

#[test]
fn iac_apply_confirm_body_is_nonempty() {
    // The body string must never be empty, even when no terraform
    // binary is installed (the wrap_body / render path assumes some
    // text to lay out).
    let body = super::cmd_static::iac_apply_confirm_body();
    assert!(!body.is_empty(), "iac confirm body must not be empty");
    assert!(
        body.contains("infrastructure") || body.contains("terraform"),
        "iac confirm body should mention the impact: {body:?}"
    );
}

#[test]
fn run_iac_command_help_with_confirmed_flag() {
    // confirmed=true on a non-apply subcommand must not consult the
    // TUI gate or the tf binary.  Smoke test that `help` still works.
    let out = super::cmd_static::run_iac_command_confirmed(Some("help"), true);
    assert!(out.starts_with("Usage:"), "help should print usage, got {out:?}");
}

#[test]
fn run_iac_command_unconfirmed_in_tui_returns_safe_message() {
    // Simulate a TUI session being active so the apply path takes the
    // defensive "modal did not resolve" branch (instead of writing to
    // stderr or blocking on stdin).
    super::tui::set_tui_session_active(true);
    let out = super::cmd_static::run_iac_command_confirmed(Some("apply"), false);
    super::tui::set_tui_session_active(false);
    // The message should mention the TUI / modal so the user knows
    // to try again.
    assert!(
        out.contains("modal") || out.contains("TUI") || out.contains("--print"),
        "expected TUI-safe message, got: {out:?}"
    );
}


// ─── Task #636 — Autonomous reflection loop integration tests ─────────────

#[test]
fn three_identical_failing_calls_inject_stuck_reminder() {
    use runtime::reflection::{StuckPattern, ToolEvent, TurnState};
    let mut ts = TurnState::with_defaults();
    ts.begin_turn();
    for _ in 0..3 {
        ts.observe_tool_event(ToolEvent::new(
            "Bash", "rm -rf /missing", Some("ENOENT".into()), None,
        ));
        ts.record_failure("Bash", "rm -rf /missing", "ENOENT");
    }
    assert!(
        matches!(ts.pending_pattern(), Some(StuckPattern::ToolLoop { .. })),
        "expected pending ToolLoop after 3 identical erroring calls"
    );
    let reminder = ts.drain_reminder_for_next_call();
    assert!(
        reminder.contains("Stuck pattern detected"),
        "system-reminder must contain `Stuck pattern detected`, got: {reminder}"
    );
    assert!(
        reminder.contains("<system-reminder>"),
        "must wrap in <system-reminder> tag, got: {reminder}"
    );
    assert!(
        reminder.contains("Previously tried in this turn"),
        "scratchpad block must accompany strategy reminder, got: {reminder}"
    );
}

#[test]
fn slash_reflect_command_emits_user_invoked_otel() {
    use commands::SlashCommand;
    let parsed = SlashCommand::parse("/reflect");
    assert!(
        matches!(parsed, Some(SlashCommand::Reflect { action: None })),
        "/reflect must parse to Reflect with no action, got {parsed:?}"
    );

    let parsed_window = SlashCommand::parse("/reflect window");
    let is_window = match parsed_window {
        Some(SlashCommand::Reflect { ref action }) => action.as_deref() == Some("window"),
        _ => false,
    };
    assert!(is_window, "/reflect window must parse to Reflect window");

    let session = runtime::Session::default();
    let result = commands::handle_slash_command(
        "/reflect",
        &session,
        runtime::CompactionConfig::default(),
    )
    .expect("/reflect must dispatch to a handler");
    assert!(
        result.message.contains("reflection"),
        "/reflect handler message must mention reflection, got: {}",
        result.message
    );
}

// ────────────────────────────────────────────────────────────────────
// Task #661 (v2.2.18 / Agent A2) — `--setup` must route to the
// alt-screen wizard, not the legacy `setup::run_setup_wizard` ASCII-box
// path.  The bug fixed by this regression suite: a user running
// `curl … | bash` was dropped into the legacy box wizard because
// `install.sh` ended with `exec anvil --setup` and `CliAction::Setup`
// was wired to `run_setup_wizard()`.  Three complementary tests:
//
//   1. Parser test — `anvil --setup` parses to `CliAction::Setup`.
//      Sanity check that the flag wiring didn't change names.
//   2. Source-level guard — assert that the dispatch site in `main.rs`
//      no longer references `run_setup_wizard`, which is the only
//      callable that emits the legacy `╔═══╗` box.  An in-process
//      runtime test would have to fork a child and capture terminal
//      output (`script` / `expect`), which doesn't run reliably on
//      every CI runner; this static check catches the same regression
//      with zero overhead.
//   3. Import guard — assert that `use setup::run_setup_wizard;` is
//      no longer present in main.rs (a regression re-adding it is
//      almost certainly trying to re-wire the legacy path).
//
// A separate integration test at
// `crates/anvil-cli/tests/setup_flag_routing.rs` spawns the actual
// binary with stdin redirected + `ANVIL_CONFIG_HOME` sandboxed and
// asserts the welcome banner from `wizard.rs` appears — the
// end-to-end proof for the same regression.
// ────────────────────────────────────────────────────────────────────

#[test]
fn cli_action_setup_flag_parses() {
    // `anvil --setup` must produce `CliAction::Setup`.
    let action = parse_args(&["--setup".to_string()])
        .expect("--setup must parse");
    assert_eq!(
        action,
        CliAction::Setup,
        "`anvil --setup` parsing changed — installer scripts may break"
    );

    // `anvil setup` (positional) must also produce `CliAction::Setup`.
    let action = parse_args(&["setup".to_string()])
        .expect("setup positional must parse");
    assert_eq!(
        action,
        CliAction::Setup,
        "`anvil setup` positional parsing changed",
    );
}

#[test]
fn main_rs_setup_dispatch_no_longer_routes_to_legacy_wizard() {
    // Read main.rs and assert the CliAction::Setup arm does NOT call
    // `run_setup_wizard` (the legacy entry point in setup.rs).  This is
    // the static-source guardrail for task #661 — if a future refactor
    // brings the legacy ASCII-box wizard back into the dispatch table,
    // this test fires.
    let main_rs = include_str!("main.rs");

    // Locate the dispatch arm for `CliAction::Setup`.  We use the
    // single-line form ("CliAction::Setup =>") because that's what the
    // dispatch table looks like; a multi-line arm would have its body
    // on subsequent lines and we'd scan those too.
    let setup_arm_idx = main_rs
        .find("CliAction::Setup =>")
        .expect("CliAction::Setup arm must exist in main.rs dispatch");

    // Grab a window large enough to cover both the single-line arm and
    // a multi-line `{ … }` block — 400 bytes is plenty.
    let end = (setup_arm_idx + 400).min(main_rs.len());
    let window = &main_rs[setup_arm_idx..end];

    // The arm body must call `run_first_run_wizard`, NOT
    // `run_setup_wizard`.  An exact-string `run_setup_wizard(` check
    // avoids false-positives on the explanatory comments above the
    // arm (those reference `run_setup_wizard` in prose with no `(`).
    assert!(
        !window.contains("run_setup_wizard("),
        "CliAction::Setup dispatch must not call run_setup_wizard() — \
         that's the legacy ASCII-box wizard.  Route to run_first_run_wizard() \
         instead. Window:\n{window}"
    );
    assert!(
        window.contains("run_first_run_wizard"),
        "CliAction::Setup dispatch must call run_first_run_wizard() so \
         installer scripts get the alt-screen wizard. Window:\n{window}"
    );
}

#[test]
fn main_rs_no_longer_imports_run_setup_wizard() {
    // The `use setup::run_setup_wizard;` import in main.rs was the
    // historical wiring for the legacy dispatch.  As of #661 the
    // import is removed (the module itself stays compiled — other
    // agents scavenge helpers from it — but the wizard entry point
    // is unused at runtime).  A regression that re-introduces the
    // import is almost certainly trying to re-wire the legacy path.
    let main_rs = include_str!("main.rs");

    // Allow `use setup;` (mod declaration) and prose mentions in
    // comments — only the specific `use setup::run_setup_wizard;`
    // top-level import is banned.
    assert!(
        !main_rs.contains("use setup::run_setup_wizard"),
        "main.rs must not `use setup::run_setup_wizard` — that import \
         is the wiring for the legacy ASCII-box wizard (task #661)."
    );
}
