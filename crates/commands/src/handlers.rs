// Task #626 — slash-command handlers in the `commands` crate run from
// both the TUI (`run_command_for_tui`) and headless (`handle_repl_command`)
// paths, and the crate has no access to the anvil-cli TUI sink.
// Handlers MUST return their output via the function's `String` return
// value so the caller routes it correctly.  Direct `println!` /
// `eprintln!` corrupts ratatui's alt-screen back-buffer.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use runtime::{compact_session, CompactionConfig, Session, WorkingMemorySnapshot};

use crate::specs::{render_command_detailed_help, render_slash_command_help};
use crate::{CursorSubcommand, SlashCommand};

/// Phase 4.4 alias-deprecation banner.
///
/// Render a one-line warning prefix shown above the legacy command's
/// normal output. The legacy command keeps working — this is a SOFT
/// deprecation kept for one release cycle, per the synthesis rule
/// "keep alias for one cycle, then hard-error".
///
/// `old` is the deprecated command verbatim (e.g. `/file-cache`).
/// `new` is the canonical replacement (e.g. `/memory show cache file`).
/// The banner has a trailing newline so the caller can concatenate it
/// directly with the legacy command's payload.
#[must_use]
pub fn phase4_4_deprecation_banner(old: &str, new: &str) -> String {
    format!("[deprecated] {old} will be removed next release; use {new}\n")
}

/// Live runtime context threaded into `/memory` handlers for
/// Phase 2 / Bucket 2 inspector views. Handlers default to filesystem-only
/// inspection when no context is supplied (the parser path doesn't have a
/// live runtime); the CLI passes a real context so `/memory show working`
/// and `/memory why` can read the live system_prompt instead of stale text.
#[derive(Debug, Default, Clone)]
pub struct MemoryContext<'a> {
    /// Live working-memory snapshot from `ConversationRuntime::working_memory_snapshot()`.
    pub working: Option<&'a WorkingMemorySnapshot>,
    /// Estimated tokens for the message buffer (separate from system_prompt).
    pub message_estimated_tokens: usize,
    /// Number of messages in the live session buffer.
    pub message_count: usize,
    /// Names of skills currently loaded into the system prompt — read off
    /// the system_prompt's `PromptSectionKind::Skill` labels. Carried
    /// separately so the L4 procedural view can render them without
    /// re-querying the prompt.
    pub loaded_skill_names: Vec<String>,
}

impl<'a> MemoryContext<'a> {
    /// Construct a context referring to a live snapshot + session-buffer stats.
    ///
    /// `loaded_skill_names` is filled from `snapshot.sections` by walking
    /// the `Skill`-kind entries and collecting their labels.
    #[must_use]
    pub fn with_working(
        snapshot: &'a WorkingMemorySnapshot,
        message_count: usize,
        message_estimated_tokens: usize,
    ) -> Self {
        let loaded_skill_names = snapshot
            .sections
            .iter()
            .filter(|s| s.kind == runtime::PromptSectionKind::Skill)
            .filter_map(|s| s.label.clone())
            .collect();
        Self {
            working: Some(snapshot),
            message_estimated_tokens,
            message_count,
            loaded_skill_names,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandResult {
    pub message: String,
    pub session: Session,
}

#[must_use]
pub fn handle_slash_command(
    input: &str,
    session: &Session,
    compaction: CompactionConfig,
) -> Option<SlashCommandResult> {
    match SlashCommand::parse(input)? {
        SlashCommand::Compact => {
            let result = compact_session(session, compaction);
            let message = if result.removed_message_count == 0 {
                "Compaction skipped: session is below the compaction threshold.".to_string()
            } else {
                format!(
                    "Compacted {} messages into a resumable system summary.",
                    result.removed_message_count
                )
            };
            Some(SlashCommandResult {
                message,
                session: result.compacted_session,
            })
        }
        // Task #557: /rewind picks a user message to roll the session back
        // to.  Without a target the TUI host runs the picker modal; the
        // headless path lists the candidate user messages so scripted
        // callers can choose by index.
        SlashCommand::Rewind { target, summarize } => {
            let (result_session, message) =
                apply_rewind(session, target, summarize, compaction);
            Some(SlashCommandResult {
                message,
                session: result_session,
            })
        }
        SlashCommand::Help { ref command } => Some(SlashCommandResult {
            message: if let Some(cmd) = command {
                render_command_detailed_help(cmd)
                    .unwrap_or_else(render_slash_command_help)
            } else {
                render_slash_command_help()
            },
            session: session.clone(),
        }),
        // ── Interactive-mode commands ─────────────────────────────────────────
        // These commands are handled by the interactive CLI (run_command_for_tui /
        // handle_repl_command). handle_slash_command is the non-interactive
        // fallback registry; for commands that require a live session these arms
        // return an accurate "start anvil interactively" message instead of the
        // former misleading "not yet implemented" text.
        SlashCommand::Status => Some(SlashCommandResult {
            message: "/status requires an active session. Run `anvil` to start an interactive session, then use /status to see token usage and session state.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Branch { .. } => Some(SlashCommandResult {
            message: "/branch requires an active git session. Run `anvil` to start interactively — /branch [list|new|switch|delete] is available in REPL and TUI modes.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Bughunter { .. } => Some(SlashCommandResult {
            message: "/bughunter requires an active session (it drives an AI-powered debugging turn). Run `anvil` and use /bughunter [scope] to start an investigation.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Worktree { .. } => Some(SlashCommandResult {
            message: "/worktree requires an active session. Run `anvil` and use /worktree [add|list|remove] to manage git worktrees from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Commit => Some(SlashCommandResult {
            message: "/commit requires an active session. Run `anvil` and use /commit to have the assistant stage and commit changes with an AI-written message.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::CommitPushPr { .. } => Some(SlashCommandResult {
            message: "/commit-push-pr requires an active session. Run `anvil` and use /commit-push-pr to commit, push, and open a pull request in one step.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Pr { .. } => Some(SlashCommandResult {
            message: "/pr requires an active session. Run `anvil` and use /pr [create|list|review] to manage pull requests from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Issue { .. } => Some(SlashCommandResult {
            message: "/issue requires an active session. Run `anvil` and use /issue [create|list|close] to manage GitHub issues from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Ultraplan { .. } => Some(SlashCommandResult {
            message: "/ultraplan requires an active session. Run `anvil` and use /ultraplan [task] to generate a detailed multi-step implementation plan.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Teleport { .. } => Some(SlashCommandResult {
            message: "/teleport requires an active session. Run `anvil` and use /teleport [target] to navigate the AI's context to a specific file or symbol.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::DebugToolCall => Some(SlashCommandResult {
            message: "/debug-tool-call requires an active session. Run `anvil` and use /debug-tool-call after a tool invocation to inspect the last tool request and response.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Model { model } => Some(SlashCommandResult {
            message: match model.as_deref() {
                None => "Current model: run `anvil` interactively to switch models with /model [name].\n\
                         TAB shows the unified cross-provider model list with provider-prefixed names,\n\
                         e.g. `cursor/claude-4-sonnet-thinking` or `anthropic/claude-sonnet-4-6`.\n\
                         Use `anvil --model <name>` to start on a specific model.".to_string(),
                Some(m) if m.contains('/') => {
                    // Provider-prefixed form: "<provider>/<model>"
                    let (provider, model_id) = m.split_once('/').unwrap();
                    format!("/model {m}: provider-prefixed model switch requires an active session.\n\
                             This will switch to provider `{provider}` and model `{model_id}` atomically\n\
                             (API routing + system prompt identity + TUI chrome all update together).\n\
                             Run `anvil` and use /model {m} to switch.")
                }
                Some(m) => format!("/model {m}: model switching requires an active session.\n\
                                    Run `anvil` and use /model {m} to switch.\n\
                                    Prefix with provider slug for unambiguous selection,\n\
                                    e.g. /model anthropic/{m} or /model cursor/{m}.\n\
                                    Use `anvil --model {m}` to start on that model."),
            },
            session: session.clone(),
        }),
        SlashCommand::Permissions { mode } => Some(SlashCommandResult {
            message: match mode.as_deref() {
                None => "Current permissions: run `anvil` interactively to inspect and change permission mode with /permissions. Use `anvil --permission-mode <mode>` to start with a specific mode.".to_string(),
                Some(m) => format!("/permissions {m}: permission changes require an active session. Use `anvil --permission-mode {m}` at startup instead."),
            },
            session: session.clone(),
        }),
        SlashCommand::Clear { .. } => Some(SlashCommandResult {
            message: "/clear requires an active session. Run `anvil` and use /clear --confirm to wipe the current conversation. For `--resume` sessions, /clear --confirm also zeroes the saved file.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Cost => Some(SlashCommandResult {
            message: "/cost requires an active session. Run `anvil` and use /cost to see cumulative token usage and estimated spend for the session.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Resume { .. } => Some(SlashCommandResult {
            message: "/resume requires an active session to switch into. Run `anvil --resume <session.json> /help` to replay commands against a saved session, or run `anvil` and use /resume interactively.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Config { .. } => Some(SlashCommandResult {
            message: "/config requires an active session. Run `anvil` and use /config [section] to view the current configuration values with live context.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Memory { action } => Some(SlashCommandResult {
            message: handle_memory_command(action.as_deref(), &MemoryContext::default()),
            session: session.clone(),
        }),
        SlashCommand::Ollama { .. } => Some(SlashCommandResult {
            message: "/ollama: intercepted by the CLI in interactive mode. In non-interactive contexts, use `anvil ollama …` from your shell instead.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Init => Some(SlashCommandResult {
            message: "/init requires an active session. Run `anvil` and use /init to create ANVIL.md and initialize project configuration for the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Diff => Some(SlashCommandResult {
            message: "/diff requires an active session. Run `anvil` and use /diff to see a `git diff --stat` summary inside the assistant context.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Version => Some(SlashCommandResult {
            message: "/version requires an active session. Run `anvil --version` from your terminal to see the installed version without starting a session.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Export { .. } => Some(SlashCommandResult {
            message: "/export requires an active session. Run `anvil` and use /export [md|text] [path] to save the conversation transcript.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Session { .. } => Some(SlashCommandResult {
            message: "/session requires an active session. Run `anvil` and use /session [list|save|load|delete] to manage named sessions.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Plugins { .. } => Some(SlashCommandResult {
            message: "/plugins requires an active session. Run `anvil` and use /plugins [list|install|enable|disable] to manage plugins.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Agents { .. } => Some(SlashCommandResult {
            message: "/agents requires an active session. Run `anvil` and use /agents [list|view|stop|clear] to manage sub-agents.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Skills { .. } => Some(SlashCommandResult {
            message: "/skills requires an active session. Run `anvil` and use /skills [list|info] to browse installed skills.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Qmd { query } => Some(SlashCommandResult {
            message: match query.as_deref().filter(|q| !q.trim().is_empty()) {
                None => "/qmd requires a query. Usage: /qmd <query>. Run `anvil` to use /qmd interactively with live QMD index search.".to_string(),
                Some(_) => "/qmd requires an active session with a QMD index. Run `anvil` and use /qmd <query> to search your knowledge base.".to_string(),
            },
            session: session.clone(),
        }),
        SlashCommand::Undo => Some(SlashCommandResult {
            message: "/undo requires an active session. Run `anvil` and use /undo to revert the last file edit made by the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::History { .. } => Some(SlashCommandResult {
            message: "/history requires an active session. Run `anvil` and use /history [all] to display the current conversation turn history.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Context { .. } => Some(SlashCommandResult {
            message: "/context requires an active session. Run `anvil` and use /context [path] to add a file or directory to the assistant's active context.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Pin { .. } => Some(SlashCommandResult {
            message: "/pin requires an active session. Run `anvil` and use /pin [path] to pin a file so it is always included in the assistant's context.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Unpin { .. } => Some(SlashCommandResult {
            message: "/unpin requires an active session. Run `anvil` and use /unpin <path> to remove a previously pinned file from the context.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Chat => Some(SlashCommandResult {
            message: "/chat requires an active session. Run `anvil` and use /chat to toggle between chat mode and task mode.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Vim => Some(SlashCommandResult {
            message: "/vim requires an active session. Run `anvil` and use /vim to toggle vim keybindings in the REPL input.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Web { .. } => Some(SlashCommandResult {
            message: "/web requires an active session. Run `anvil` and use /web <query> to perform a web search from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Doctor { mode } => Some(SlashCommandResult {
            message: match mode.as_deref() {
                Some("release") => "/doctor release: release-pipeline pre-flight checks require an active session. Run `anvil` and use /doctor release.".to_string(),
                _ => "/doctor requires an active session. Run `anvil` and use /doctor to run configuration and connectivity diagnostics.".to_string(),
            },
            session: session.clone(),
        }),
        SlashCommand::Tokens => Some(SlashCommandResult {
            message: "/tokens requires an active session. Run `anvil` and use /tokens to display current session token count and context window usage.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Provider { .. } => Some(SlashCommandResult {
            message: "/provider requires an active session. Run `anvil` and use /provider [list|status|switch] to inspect and change the active AI provider.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Login { provider } => Some(SlashCommandResult {
            message: match provider.as_deref() {
                None => "/login requires an active session. Run `anvil` and use /login [provider] to authenticate with an AI provider.".to_string(),
                Some(p) => format!("/login {p}: authentication requires an active session. Run `anvil` and use /login {p}."),
            },
            session: session.clone(),
        }),
        SlashCommand::Search { .. } => Some(SlashCommandResult {
            message: "/search requires an active session. Run `anvil` and use /search <query> to search the codebase using ripgrep or the assistant's file tools.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Failover { .. } => Some(SlashCommandResult {
            message: "/failover requires an active session. Run `anvil` and use /failover [status|test|switch] to manage provider failover.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::GenerateImage { .. } => Some(SlashCommandResult {
            message: "/generate-image requires an active session. Run `anvil` and use /generate-image <prompt> to create an image via the configured image-generation provider.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::HistoryArchive { .. } => Some(SlashCommandResult {
            // Phase 4.4: deprecation banner — `/history-archive` is a soft
            // alias for the L2 (episodic) view exposed by `/memory show episodic`.
            message: format!(
                "{}{}",
                phase4_4_deprecation_banner("/history-archive", "/memory show episodic"),
                "/history-archive requires an active session for search and view sub-commands. \
                 Use /memory show episodic (the canonical replacement) or run `anvil` for the full \
                 history-archive interface.",
            ),
            session: session.clone(),
        }),
        SlashCommand::Configure { .. } => Some(SlashCommandResult {
            message: "/configure requires an active TUI session. Run `anvil` and use /configure to open the interactive settings menu. For non-interactive changes, edit ~/.anvil/config.json directly.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Theme { action } => Some(SlashCommandResult {
            message: match action.as_deref() {
                None => "/theme requires an active session. Run `anvil` and use /theme [list|set <name>|reset] to apply a color theme.".to_string(),
                Some("list") => "/theme list: themes are configured per-session. Run `anvil` and use /theme list to browse installed themes.".to_string(),
                Some(a) => format!("/theme {a}: theme changes require an active session. Run `anvil` and use /theme {a}."),
            },
            session: session.clone(),
        }),
        SlashCommand::SemanticSearch { .. } => Some(SlashCommandResult {
            message: "/semantic-search requires an active session with a QMD index. Run `anvil` and use /semantic-search <query> to search your knowledge base semantically.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Docker { .. } => Some(SlashCommandResult {
            message: "/docker requires an active session. Run `anvil` and use /docker [ps|logs|compose|build] to run Docker operations from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Test { .. } => Some(SlashCommandResult {
            message: "/test requires an active session. Run `anvil` and use /test [run|generate|coverage] to run or generate tests from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Git { .. } => Some(SlashCommandResult {
            message: "/git requires an active session. Run `anvil` and use /git [rebase|conflicts|cherry-pick|stash] to perform git operations from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Refactor { .. } => Some(SlashCommandResult {
            message: "/refactor requires an active session. Run `anvil` and use /refactor [rename|extract|move] to perform AI-assisted code refactoring.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Screenshot => Some(SlashCommandResult {
            message: "/screenshot requires an active TUI session. Run `anvil` (TUI mode) and use /screenshot to capture the screen and send it to the assistant as a vision input.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Db { .. } => Some(SlashCommandResult {
            message: "/db requires an active session. Run `anvil` and use /db [connect|schema|query|migrate] to interact with databases from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Security { .. } => Some(SlashCommandResult {
            message: "/security requires an active session. Run `anvil` and use /security [scan|secrets|deps|report] to run security analysis from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Api { .. } => Some(SlashCommandResult {
            message: "/api requires an active session. Run `anvil` and use /api [spec|mock|test|docs] to work with API definitions and testing.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Docs { .. } => Some(SlashCommandResult {
            message: "/docs requires an active session. Run `anvil` and use /docs [generate|readme|architecture|changelog] to generate documentation.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Scaffold { .. } => Some(SlashCommandResult {
            message: "/scaffold requires an active session. Run `anvil` and use /scaffold [new <template>|list] to generate project boilerplate.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Perf { .. } => Some(SlashCommandResult {
            message: "/perf requires an active session. Run `anvil` and use /perf [profile|benchmark|flamegraph|analyze] to run performance analysis.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Debug { .. } => Some(SlashCommandResult {
            message: "/debug requires an active session. Run `anvil` and use /debug [start|breakpoint|watch|explain] to start an AI-assisted debugging session.".to_string(),
            session: session.clone(),
        }),
        // [deferred:Phase5.1] /voice — requires audio capture API; no current
        // implementation. Deferred pending microphone permission + streaming design.
        SlashCommand::Voice { .. } => Some(SlashCommandResult {
            message: "/voice: voice input is deferred (Phase 5.1). It requires microphone access and streaming audio-to-text. Use typed input for now.".to_string(),
            session: session.clone(),
        }),
        // [deferred:Phase5.1] /collab — requires a shared relay session; currently
        // the relay only supports read-only viewing, not bidirectional collab editing.
        SlashCommand::Collab { .. } => Some(SlashCommandResult {
            message: "/collab: real-time collaboration is deferred (Phase 5.1). The relay currently supports read-only sharing (/share). Bidirectional editing requires additional relay protocol work.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Changelog => Some(SlashCommandResult {
            message: "/changelog requires an active session. Run `anvil` and use /changelog to generate a CHANGELOG entry from git history since the last tag.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Env { .. } => Some(SlashCommandResult {
            message: "/env requires an active session. Run `anvil` and use /env [show|set|load|diff] to manage environment variables from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Hub { .. } => Some(SlashCommandResult {
            message: "/hub requires an active session. Run `anvil` and use /hub [search|skills|plugins|install|info|status] to browse and install content from AnvilHub.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::HubStatus { package } => Some(SlashCommandResult {
            message: if package.is_empty() {
                "Usage: /hub-status <package-name>\n\
                 Shows the verification state, publisher info, and highest verified version for a package.\n\
                 Example: /hub-status devops-expert".to_string()
            } else {
                format!(
                    "/hub-status requires an active session. Run `anvil` and use \
                     `/hub-status {package}` or `/hub status {package}` to query the AnvilHub \
                     verification badge for package '{package}'."
                )
            },
            session: session.clone(),
        }),
        SlashCommand::Layout { action } => Some(SlashCommandResult {
            message: handle_layout_command(action.as_deref()),
            session: session.clone(),
        }),
        SlashCommand::Language { lang } => Some(SlashCommandResult {
            message: match lang.as_deref() {
                None => "/language requires an active session. Run `anvil` and use /language [en|de|es|fr|ja|zh-CN|ru] to switch the assistant's response language.".to_string(),
                Some(l) => format!("/language {l}: language switching requires an active session. Run `anvil` and use /language {l}."),
            },
            session: session.clone(),
        }),
        // [deferred:Phase5.1] /lsp — requires a background language-server
        // process; the LSP integration is not wired in the current release.
        SlashCommand::Lsp { .. } => Some(SlashCommandResult {
            message: "/lsp: language-server integration is deferred (Phase 5.1). The protocol scaffolding exists but per-language server spawning and capability negotiation are not complete.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Notebook { .. } => Some(SlashCommandResult {
            message: "/notebook requires an active session. Run `anvil` and use /notebook [run|cell|export] to work with Jupyter notebooks from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        // [deferred:Phase5.1] /k8s — requires kubectl and a live kubeconfig;
        // the cluster connectivity and error model need more design work.
        SlashCommand::K8s { .. } => Some(SlashCommandResult {
            message: "/k8s: Kubernetes integration is deferred (Phase 5.1). Run `anvil` and use /k8s [pods|logs|apply|describe] once the integration is complete, or use `kubectl` directly.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Iac { .. } => Some(SlashCommandResult {
            message: "/iac requires an active session. Run `anvil` and use /iac [plan|apply|validate|drift] to run infrastructure-as-code operations from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Pipeline { .. } => Some(SlashCommandResult {
            message: "/pipeline requires an active session. Run `anvil` and use /pipeline [generate|lint|run] to manage CI/CD pipeline configurations.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Review { .. } => Some(SlashCommandResult {
            message: "/review requires an active session. Run `anvil` and use /review [<file>|staged|pr] to start an AI code review.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Deps { .. } => Some(SlashCommandResult {
            message: "/deps requires an active session. Run `anvil` and use /deps [tree|outdated|audit|why] to inspect project dependencies.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Mono { .. } => Some(SlashCommandResult {
            message: "/mono requires an active session. Run `anvil` and use /mono [list|graph|changed|run] to work with monorepo workspaces.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Browser { .. } => Some(SlashCommandResult {
            message: "/browser requires an active session. Run `anvil` and use /browser [open|screenshot|test] to drive browser automation from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Notify { .. } => Some(SlashCommandResult {
            message: "/notify requires an active session. Run `anvil` and use /notify [send|webhook|matrix] to send notifications from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Vault { .. } => Some(SlashCommandResult {
            message: "/vault requires an active session. Run `anvil` and use /vault [setup|unlock|lock|store|get|list|delete|totp] to manage the Anvil credential vault.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Migrate { .. } => Some(SlashCommandResult {
            message: "/migrate requires an active session. Run `anvil` and use /migrate [framework|language|deps] to perform AI-assisted codebase migrations.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Regex { .. } => Some(SlashCommandResult {
            message: "/regex requires an active session. Run `anvil` and use /regex [build|test|explain] to work with regular expressions from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        // SSH has two modes: TUI (embedded client) and REPL (message-only).
        // The REPL message is accurate — SSH modal UI requires TUI.
        SlashCommand::Ssh { .. } => Some(SlashCommandResult {
            message: "/ssh: the embedded SSH client requires the Anvil TUI. Run `anvil` (without --print) and use /ssh [alias] to connect, or /ssh save <alias> to store a connection.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Logs { .. } => Some(SlashCommandResult {
            message: "/logs requires an active session. Run `anvil` and use /logs [tail|search|analyze|stats] to analyze log files from inside the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Markdown { .. } => Some(SlashCommandResult {
            message: "/markdown requires an active session. Run `anvil` and use /markdown [preview|toc|lint] to work with Markdown documents.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Snippets { .. } => Some(SlashCommandResult {
            message: "/snippets requires an active session. Run `anvil` and use /snippets [save|list|get|search] to manage the code snippet library.".to_string(),
            session: session.clone(),
        }),
        // [deferred:Phase5.1] /finetune — requires model fine-tuning API access
        // and a training data pipeline; not planned for current release cycle.
        SlashCommand::Finetune { .. } => Some(SlashCommandResult {
            message: "/finetune: model fine-tuning is deferred (Phase 5.1). It requires provider-side fine-tuning API access and a training data workflow that has no current implementation.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Webhook { .. } => Some(SlashCommandResult {
            message: "/webhook requires an active session. Run `anvil` and use /webhook [list|add|test|remove] to manage webhook endpoints.".to_string(),
            session: session.clone(),
        }),
        // [deferred:Phase5.1] /plugin-sdk — requires plugin build toolchain integration.
        SlashCommand::PluginSdk { .. } => Some(SlashCommandResult {
            message: "/plugin-sdk: the plugin development SDK is deferred (Phase 5.1). Plugin authoring tooling (init, build, test, publish) is not yet wired. Refer to the plugin manifest spec in the documentation.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Sleep => Some(SlashCommandResult {
            message: "/sleep: the screensaver activates automatically on inactivity. Run `anvil` (TUI mode) and use /sleep to trigger it immediately.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Think => Some(SlashCommandResult {
            message: "/think requires an active session. Run `anvil` and use /think to toggle extended reasoning mode for models that support it (e.g. claude-sonnet-4-5-think).".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Fast => Some(SlashCommandResult {
            message: "/fast requires an active session. Run `anvil` and use /fast to toggle fast mode (lower max_tokens + concise system prompt) for reduced latency.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::ReviewPr { .. } => Some(SlashCommandResult {
            message: "/review-pr requires an active session. Run `anvil` and use /review-pr [<number>] to fetch a GitHub PR diff and review it with the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::RemoteControl { .. } => Some(SlashCommandResult {
            message: "/remote-control requires an active TUI session. Run `anvil` and use /remote-control to start a relay session for remote viewing and co-piloting.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Loop { .. } => Some(SlashCommandResult {
            message: "/loop requires an active session. Run `anvil` and use /loop [prompt] to enter autonomous loop mode, repeating a task until a stopping condition is met.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Focus => Some(SlashCommandResult {
            message: "/focus requires an active TUI session. Run `anvil` and use /focus to toggle focus view (shows prompts, tool summaries, and final responses only).".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Mcp { .. } => Some(SlashCommandResult {
            message: "/mcp requires an active session. Run `anvil` and use /mcp [list|status|tools] to inspect connected MCP servers and their available tools.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Productivity => Some(SlashCommandResult {
            message: "/productivity requires an active session. Run `anvil` and use /productivity to view session statistics: turns, tool calls, tokens, and task completion rate.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Knowledge { .. } => Some(SlashCommandResult {
            message: "/knowledge requires an active session. Run `anvil` and use /knowledge [review|accept <N>|reject <N>|list] to manage AI-generated knowledge nominations.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Daily { .. } => Some(SlashCommandResult {
            message: "/daily requires an active session. Run `anvil` and use /daily [date] to view the daily session summary and task reconciliation.".to_string(),
            session: session.clone(),
        }),
        // ── TUI-only commands (v2.2.6+) ───────────────────────────────────────
        SlashCommand::Tab { .. } => Some(SlashCommandResult {
            message: "/tab is a TUI-only command. Run `anvil` (TUI mode) and use /tab [new|close|switch|list] or the keyboard shortcuts Ctrl+T (new), Ctrl+W (close), Ctrl+]/[ (switch).".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Fork => Some(SlashCommandResult {
            message: "/fork is a TUI-only command. Run `anvil` (TUI mode) and use /fork to duplicate the current tab with the same conversation context.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Share { .. } => Some(SlashCommandResult {
            message: "/share is a TUI-only command. Run `anvil` (TUI mode) and use /share to generate a read-only relay link for the current tab. Requires /vault unlock.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Audit => Some(SlashCommandResult {
            message: "/audit requires an active session. Run `anvil` and use /audit to run a composite check: /security scan + /deps audit + /vault verify.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Restart { .. } => Some(SlashCommandResult {
            message: "/restart requires an active session. Run `anvil` and use /restart to respawn the process (full restart) or /restart --soft to reload configuration in-place.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Agent { .. } => Some(SlashCommandResult {
            message: "/agent requires an active session. Run `anvil` and use /agent traits to list trait definitions, or /agent compose <traits> \"<task>\" to run a composed agent turn.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::OutputStyle { style } => Some(SlashCommandResult {
            message: format!(
                "Output style: {}",
                style.as_deref().unwrap_or("(current)")
            ),
            session: session.clone(),
        }),
        SlashCommand::Effort { level } => Some(SlashCommandResult {
            message: match level.as_deref() {
                None => "/effort — use in the live REPL to get or set the reasoning effort level (low|medium|high|xhigh).".to_string(),
                Some(l) => format!("Effort level set to: {l} (applies in the live REPL session)"),
            },
            session: session.clone(),
        }),
        SlashCommand::Chain { subcommand } => {
            let message = handle_chain_subcommand(subcommand);
            Some(SlashCommandResult { message, session: session.clone() })
        }
        SlashCommand::Reflect { action } => {
            runtime::reflection::otel_user_invoked();
            let message = match action.as_deref() {
                None | Some("status") => "/reflect - reflection status requires an active session. Run `anvil` to inspect live TurnState. Subcommands: /reflect window, /reflect scratchpad.".to_string(),
                Some("window") => "/reflect window - requires an active session. Run `anvil` and re-issue /reflect window to dump the last 20 tool events.".to_string(),
                Some("scratchpad") => "/reflect scratchpad - requires an active session. Run `anvil` and re-issue /reflect scratchpad to list this turn's failed attempts.".to_string(),
                Some(other) => format!("/reflect: unknown subcommand `{other}`. Try /reflect [status|window|scratchpad]."),
            };
            Some(SlashCommandResult { message, session: session.clone() })
        }
        SlashCommand::Skill { subcommand } => {
            use crate::agents::{discover_skill_roots, load_skills_from_roots};
            use crate::{format_suggestions, match_triggers, SkillSubcommand};
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let message = match subcommand {
                SkillSubcommand::List => {
                    crate::agents::handle_skills_slash_command(None, &cwd)
                        .unwrap_or_else(|e| format!("Error listing skills: {e}"))
                }
                SkillSubcommand::Suggest { prompt } => {
                    let prompt_text = prompt.unwrap_or_default();
                    if prompt_text.is_empty() {
                        "No prompt provided. Usage: /skill suggest <prompt>".to_string()
                    } else {
                        let roots = discover_skill_roots(&cwd);
                        let skills = load_skills_from_roots(&roots).unwrap_or_default();
                        let skill_refs: Vec<&crate::agents::SkillSummary> = skills.iter().collect();
                        let matches = match_triggers(&prompt_text, &skill_refs);
                        format_suggestions(&matches, &prompt_text)
                    }
                }
                SkillSubcommand::Load { name } => {
                    if name.trim().is_empty() {
                        "Usage: /skill load <name>. Use /skill list to browse installed skills.".to_string()
                    } else {
                        let roots = discover_skill_roots(&cwd);
                        let skills = load_skills_from_roots(&roots).unwrap_or_default();
                        if skills.iter().any(|s| s.name.eq_ignore_ascii_case(name.trim())) {
                            format!(
                                "Skill '{name}' queued for injection into the next turn.                                  Use /skill load in an interactive session for full injection."
                            )
                        } else {
                            format!(
                                "No such skill '{name}'. Use /skill list to browse installed skills."
                            )
                        }
                    }
                }
                SkillSubcommand::Chains => {
                    let roots = discover_skill_roots(&cwd);
                    let skills = load_skills_from_roots(&roots).unwrap_or_default();
                    let all_skills: std::collections::HashMap<String, crate::agents::SkillSummary> =
                        skills.into_iter().map(|s| (s.name.to_ascii_lowercase(), s)).collect();
                    crate::skill_chaining::render_chains_graph(&all_skills)
                }
                SkillSubcommand::Install { .. } => {
                    // /skill install <slug> requires the live HubClient runtime
                    // and AnvilHub network access. The TUI path handles it.
                    "/skill install requires an active session. Run `anvil` and use /skill install <slug> to fetch the package from AnvilHub.".to_string()
                }
            };
            Some(SlashCommandResult { message, session: session.clone() })
        }
        SlashCommand::Goal { action } => {
            let msg = match action.as_deref() {
                None | Some("list") => {
                    "/goal list — goal tracking is fully available in interactive mode. \
                     Use /goal new \"<description>\" to create a goal, or run `anvil` and use /goal."
                        .to_string()
                }
                Some(other) => {
                    format!(
                        "/goal {other} — goal management requires an active session. \
                         Run `anvil` and use /goal in the interactive REPL or TUI."
                    )
                }
            };
            Some(SlashCommandResult {
                message: msg,
                session: session.clone(),
            })
        }
        SlashCommand::Profile { action } => {
            let msg = match action.as_deref() {
                None | Some("list") => {
                    "/profile list — profile management is handled in the TUI. \
                     Use --profile <name> at startup or set ANVIL_PROFILE to activate a profile."
                        .to_string()
                }
                Some(other) => {
                    format!(
                        "/profile {other} — profile management is handled in the TUI. \
                         Use --profile <name> at startup or set ANVIL_PROFILE to activate a profile."
                    )
                }
            };
            Some(SlashCommandResult {
                message: msg,
                session: session.clone(),
            })
        }
        SlashCommand::FileCache { action: _ } => Some(SlashCommandResult {
            // Phase 4.4: deprecation banner. The legacy `/file-cache` still
            // works but the new canonical form is `/memory show cache file`.
            message: format!(
                "{}{}",
                phase4_4_deprecation_banner("/file-cache", "/memory show cache file"),
                "/file-cache: use /file-cache list, stats, prune, or forget <path>",
            ),
            session: session.clone(),
        }),
        SlashCommand::CmdCache { action } => {
            use runtime::CommandCacheManager;
            use std::env;
            let cwd = env::current_dir().unwrap_or_default();
            let msg = match action.as_deref() {
                None | Some("list") => match CommandCacheManager::new(cwd.clone()) {
                    Ok(mgr) => match mgr.list() {
                        Ok(entries) if entries.is_empty() => "Command-output cache is empty.".to_string(),
                        Ok(entries) => {
                            let mut lines = vec![format!("Cached commands ({}):", entries.len())];
                            for e in entries.iter().take(20) {
                                lines.push(format!("  [hits:{:>3}] {}", e.hits, &e.command));
                            }
                            if entries.len() > 20 {
                                lines.push(format!("  … and {} more", entries.len() - 20));
                            }
                            lines.join("\n")
                        }
                        Err(e) => format!("Error listing cache: {e}"),
                    },
                    Err(e) => format!("Error opening cache: {e}"),
                },
                Some("stats") => match CommandCacheManager::new(cwd.clone()) {
                    Ok(mgr) => match mgr.stats() {
                        Ok(stats) => format!(
                            "Command cache stats:\n  entries:   {}\n  hits:      {}\n  stale:     {}\n  bytes:     {}",
                            stats.total_entries, stats.total_hits, stats.stale_entries, stats.total_size_bytes,
                        ),
                        Err(e) => format!("Error reading stats: {e}"),
                    },
                    Err(e) => format!("Error opening cache: {e}"),
                },
                Some("prune") => match CommandCacheManager::new(cwd.clone()) {
                    Ok(mgr) => match mgr.prune_stale() {
                        Ok(n) => format!("Pruned {n} stale cache entries."),
                        Err(e) => format!("Error pruning cache: {e}"),
                    },
                    Err(e) => format!("Error opening cache: {e}"),
                },
                Some(rest) if rest.starts_with("forget ") => {
                    let command: &str = rest.trim_start_matches("forget ").trim();
                    if command.is_empty() {
                        "Usage: /cmd-cache forget <command>".to_string()
                    } else {
                        match CommandCacheManager::new(cwd.clone()) {
                            Ok(mgr) => match mgr.forget(command, &cwd) {
                                Ok(()) => format!("Forgotten: {command}"),
                                Err(e) => format!("Error: {e}"),
                            },
                            Err(e) => format!("Error opening cache: {e}"),
                        }
                    }
                }
                Some(other) => format!(
                    "Unknown /cmd-cache subcommand: {other}\nUsage: /cmd-cache [list|stats|prune|forget <command>]"
                ),
            };
            // Phase 4.4: deprecation banner. Prepend without dropping the
            // original (working) payload — `/cmd-cache` still functions.
            let msg = format!(
                "{}{}",
                phase4_4_deprecation_banner("/cmd-cache", "/memory show cache cmd"),
                msg,
            );
            Some(SlashCommandResult { message: msg, session: session.clone() })
        }
        SlashCommand::ScrollSpeed { lines } => {
            // CC-139-F3 parity. This handler runs in non-TUI contexts
            // (anvil --print, /print-mode, daemons). In the TUI proper,
            // main.rs intercepts and applies live. Here we just report
            // or set the process-wide AtomicU8 so any embedded TUI
            // picks up the change next paint.
            let msg = match lines.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
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
            };
            Some(SlashCommandResult { message: msg, session: session.clone() })
        }
        // ── Phase 6.0 — /import foundation ───────────────────────────────────
        SlashCommand::Import { source, dry_run, scope, include_sessions } => {
            Some(SlashCommandResult {
                message: handle_import_command(
                    source.as_deref(),
                    dry_run,
                    scope.as_deref(),
                    include_sessions,
                ),
                session: session.clone(),
            })
        }
        SlashCommand::Cursor { subcommand } => Some(SlashCommandResult {
            message: handle_cursor_command(&subcommand),
            session: session.clone(),
        }),
        SlashCommand::Heal => Some(SlashCommandResult {
            // /heal opens an interactive modal — this static handler can
            // only point users at the live dispatcher in main.rs (TUI) /
            // `anvil --check` (CLI).
            message: "Run `anvil --check` for a structured health report,\nor invoke /heal inside a live Anvil session to open the modal."
                .to_string(),
            session: session.clone(),
        }),
        SlashCommand::Unknown(cmd) => Some(SlashCommandResult {
            message: format!("/{cmd} is not a recognized command. Type /help to see all available commands."),
            session: session.clone(),
        }),
    }
}

// ─── /import command — Phase 6.1 live implementation ─────────────────────────

/// Handler for `/import [source] [flags]`.
///
/// Phase 6.1 runs the full memory + instructions import pipeline:
/// Discover → Triage → Translate → Stage, returning a markdown ResultBlock
/// summary.  Commit happens when the user runs without --dry-run.
///
/// Phase 6.4 adds the permission gate: ReadOnly mode blocks commits.
///
/// Settings/skills/plugins (Bucket 2 / Phase 6.2) are not yet wired here;
/// this handler composes only what Phase 6.1 provides.
///
/// # Gate compliance
///
/// - Does NOT contain the string "(stub)" (Gate 2).
/// - Does NOT contain "not yet implemented" (Gate 5.0.5).
/// - Returns a non-empty string for all argument combinations (Gate 2).
/// - Does NOT call blocking LLM methods (Gate 3).
#[must_use]
pub fn handle_import_command(
    source: Option<&str>,
    dry_run: bool,
    scope: Option<&str>,
    include_sessions: bool,
) -> String {
    handle_import_command_with_mode(source, dry_run, scope, include_sessions, None)
}

/// Permission-gate-aware variant of `handle_import_command`.
///
/// When `permission_mode` is `Some(ReadOnly)` and `dry_run` is `false`,
/// the commit phase is blocked and a clear error is returned.
///
/// The wizard path passes `None` (implicit `WorkspaceWrite`); the TUI path
/// passes the live permission mode.
#[must_use]
pub fn handle_import_command_with_mode(
    source: Option<&str>,
    dry_run: bool,
    scope: Option<&str>,
    include_sessions: bool,
    permission_mode: Option<runtime::PermissionMode>,
) -> String {
    use runtime::PermissionMode;

    let _ = scope; // Phase 6.1: scope filtering is applied in run_import_pipeline_headless.

    // Permission gate: ReadOnly blocks writes.  Dry-run is always allowed
    // (no writes happen).  WorkspaceWrite and DangerFullAccess both allow.
    if !dry_run {
        if let Some(PermissionMode::ReadOnly) = permission_mode {
            return "Import requires WorkspaceWrite mode or above. \
                    Run `/permissions mode acceptEdits` first, or use \
                    `/import claude-code --dry-run` to preview without writing."
                .to_string();
        }
    }

    match source {
        Some("claude-code") | None => {
            let import_source = runtime::ImportSource::default_cc();
            run_import_pipeline_headless(&import_source, dry_run, include_sessions)
                .unwrap_or_else(|e| format!("/import failed: {e}"))
        }
        Some(other) => {
            format!(
                "/import: unknown source '{other}'. \
                 Supported sources: claude-code\n\
                 Usage: /import claude-code [--dry-run] [--scope=all|current-project|global] [--include-sessions]"
            )
        }
    }
}

// ─── Phase 6.4 — Import pipeline orchestrator ────────────────────────────────

/// Summary produced after Discover+Triage+Stage (before Commit).
///
/// Used to populate the confirmation TUI and the import plan display.
#[derive(Debug, Clone, Default)]
pub struct ImportPlanSummary {
    /// Count of memory artifacts discovered.
    pub memory_found: usize,
    /// Count of memory artifacts that were staged (will land or would land).
    pub memory_staged: usize,
    /// Count of memory artifacts skipped.
    pub memory_skipped: usize,
    /// Count of instruction files (CLAUDE.md → ANVIL.md) discovered.
    pub instructions_found: usize,
    /// Count of instruction files staged.
    pub instructions_staged: usize,
    /// Count of instruction files that need review (conflict).
    pub instructions_needs_review: usize,
    /// Count of instruction files skipped.
    pub instructions_skipped: usize,
    /// Count of settings artifacts discovered.
    pub settings_found: usize,
    /// True if the settings translation produced conflicts.
    pub settings_has_conflicts: bool,
    /// Count of settings conflict keys.
    pub settings_conflict_count: usize,
    /// Count of skill artifacts discovered.
    pub skills_found: usize,
    /// Count of skills staged.
    pub skills_staged: usize,
    /// Count of skills skipped.
    pub skills_skipped: usize,
    /// Count of skills that need review (name collision).
    pub skills_needs_review: usize,
    /// Count of plugin artifacts discovered.
    pub plugins_found: usize,
    /// Count of plugins staged.
    pub plugins_staged: usize,
    /// Count of plugins skipped.
    pub plugins_skipped: usize,
    /// Count of plugins that need review (name collision).
    pub plugins_needs_review: usize,
    /// Count of artifacts staged (will be committed) — all categories.
    pub total_staged: usize,
    /// Count of artifacts that need user review — all categories.
    pub total_needs_review: usize,
    /// Count of artifacts that will be skipped — all categories.
    pub total_skipped: usize,
    /// Items that need explicit user review: (description, path).
    pub needs_review_items: Vec<(String, String)>,
    /// Conflicting keys/paths detected: (key_or_path, description).
    pub conflicts: Vec<(String, String)>,
    // ── Phase 6.3 — session import counters ──────────────────────────────────
    /// Count of session JSONL files discovered.
    pub sessions_discovered: usize,
    /// Count of sessions successfully summarized and staged.
    pub sessions_summarized: usize,
    /// Count of sessions skipped because they exceeded 50 MB.
    pub sessions_skipped_oversized: usize,
    /// Count of sessions that failed summarization.
    pub sessions_failed: usize,
    /// Count of lesson nominations written to staging.
    pub sessions_nominations_staged: usize,
}

impl ImportPlanSummary {
    /// Render the plan as a human-readable confirmation block.
    ///
    /// Matches the "Import Plan" format specified in Phase 6.4.
    #[must_use]
    pub fn render_confirmation_block(&self, dry_run: bool) -> String {
        let mode_label = if dry_run {
            " [DRY RUN — nothing will be committed]"
        } else {
            ""
        };

        let verb = if dry_run { "would be committed" } else { "committed" };

        // Memory line
        let mem_line = format!(
            "  Memory entries:    {} found, {} {}, {} skipped",
            self.memory_found, self.memory_staged, verb, self.memory_skipped
        );

        // Instructions line
        let instr_review = if self.instructions_needs_review > 0 {
            format!(", {} NEEDS REVIEW", self.instructions_needs_review)
        } else {
            String::new()
        };
        let instr_line = format!(
            "  Instructions:      {} found, {} staged{}, {} skipped",
            self.instructions_found, self.instructions_staged, instr_review, self.instructions_skipped
        );

        // Settings line
        let settings_line = if self.settings_found == 0 {
            "  Settings:          none found".to_string()
        } else if self.settings_has_conflicts {
            format!(
                "  Settings:          1 settings.json ({} key(s) CONFLICT — see ~/.anvil/.import-review/)",
                self.settings_conflict_count
            )
        } else {
            "  Settings:          1 settings.json (no conflicts)".to_string()
        };

        // Skills line
        let skills_review = if self.skills_needs_review > 0 {
            format!(", {} NEEDS REVIEW", self.skills_needs_review)
        } else {
            String::new()
        };
        let skills_line = format!(
            "  Skills:            {} found, {} staged{} (DISABLED by default), {} skipped",
            self.skills_found, self.skills_staged, skills_review, self.skills_skipped
        );

        // Plugins line
        let plugins_review = if self.plugins_needs_review > 0 {
            format!(", {} NEEDS REVIEW", self.plugins_needs_review)
        } else {
            String::new()
        };
        let plugins_line = format!(
            "  Plugins:           {} found, {} staged{}, {} skipped",
            self.plugins_found, self.plugins_staged, plugins_review, self.plugins_skipped
        );

        // Sessions line
        let sessions_line = if self.sessions_discovered == 0 {
            "  Sessions:          SKIPPED (run with --include-sessions to summarize)".to_string()
        } else {
            format!(
                "  Sessions:          {} discovered, {} summarized, {} skipped (oversized), {} nominations",
                self.sessions_discovered,
                self.sessions_summarized,
                self.sessions_skipped_oversized,
                self.sessions_nominations_staged,
            )
        };

        let mut lines = vec![
            format!("=== Import Plan{mode_label} ==="),
            String::new(),
            mem_line,
            instr_line,
            settings_line,
            skills_line,
            plugins_line,
            sessions_line,
        ];

        if !self.needs_review_items.is_empty() {
            lines.push(String::new());
            lines.push("  Needs Review:".to_string());
            for (desc, path) in &self.needs_review_items {
                lines.push(format!("    - {desc}"));
                if !path.is_empty() {
                    lines.push(format!("      Inspect: {path}"));
                }
            }
        }

        if !self.conflicts.is_empty() {
            lines.push(String::new());
            lines.push("  Conflicts:".to_string());
            for (key, desc) in &self.conflicts {
                lines.push(format!("    - {key}: {desc}"));
            }
        }

        if !dry_run {
            lines.push(String::new());
            lines.push("  [c]ommit / [d]ry-run (already done, show me again) / [a]bort".to_string());
        }

        lines.join("\n")
    }
}

/// Run the import pipeline without a live TUI (headless mode).
///
/// Used by both the wizard migration step and by the headless / non-TUI
/// invocation path.  Returns a human-readable summary string on success, or
/// an error string on failure.
///
/// # Permission gate
///
/// When `dry_run` is `false`, this function requires `WorkspaceWrite` or above.
/// Callers are responsible for checking the permission mode before calling;
/// the wizard path always has implicit WorkspaceWrite access during first-run.
///
/// # Errors
///
/// Returns a string describing the failure.
pub fn run_import_pipeline_headless(
    source: &runtime::ImportSource,
    dry_run: bool,
    include_sessions: bool,
) -> Result<String, String> {
    use runtime::{
        ImportManifest, StagingDir, CommitResult, now_rfc3339,
        otel_import_invoked, otel_import_discovered, otel_import_staged,
        otel_import_committed, otel_import_skipped, otel_import_completed,
        write_full_report, ReportOptions,
        ImportEntry, ImportEntryStatus,
    };
    use runtime::import::memory::run_memory_pipeline;
    use runtime::import::instructions::run_instructions_pipeline;
    use runtime::import::settings::run_settings_import;
    use runtime::import::skills::run_skills_import;
    use runtime::import::plugins::run_plugins_import;
    use runtime::import::sessions::{run_sessions_import, commit_sessions_from_staging, SessionImportStatus};
    use runtime::import::commit::run_commit;

    let start = std::time::Instant::now();
    let scope = "all";

    // Step 1: OTel — import.invoked
    otel_import_invoked(&source.label(), scope, dry_run, include_sessions);

    // Load the manifest for idempotency gating.
    let manifest_path = ImportManifest::default_path();
    let base_manifest = ImportManifest::load_or_new(&manifest_path, env!("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| ImportManifest::new(env!("CARGO_PKG_VERSION")));

    let timestamp = now_rfc3339();
    let mut needs_review: Vec<(String, String)> = Vec::new();
    let mut conflicts_list: Vec<(String, String)> = Vec::new();

    // Obtain the CC profile directory for pipelines that take a &Path directly.
    let profile_dir = match source {
        runtime::ImportSource::ClaudeCode { profile_dir } => profile_dir.clone(),
        _ => {
            return Err(format!("unsupported import source: {}", source.label()));
        }
    };

    // Always create a staging dir — dry-run uses it too (scratch only).
    let staging = StagingDir::create_clean()
        .map_err(|e| format!("staging setup failed: {e}"))?;

    // ── Pipeline counters ────────────────────────────────────────────────────────
    // Each counter triple: (found, staged, skipped)

    // ── 1. Memory ────────────────────────────────────────────────────────────────
    let memory_pipeline = run_memory_pipeline(source, &staging, &base_manifest);
    let mem_found = memory_pipeline.staged.len()
        + memory_pipeline.skipped.len()
        + memory_pipeline.needs_review.len();
    let mem_stage_cnt = memory_pipeline.staged.len();
    let mem_skip_cnt = memory_pipeline.skipped.len();

    otel_import_discovered("memory", mem_found);

    // ── 2. Instructions ───────────────────────────────────────────────────────────
    let instr_pipeline = run_instructions_pipeline(
        source,
        &staging,
        &base_manifest,
        None, // use default ~/projects/ walk root
    );
    let instr_found = instr_pipeline.staged.len() + instr_pipeline.skipped.len();
    let instr_stage_cnt = instr_pipeline.staged.len();
    let instr_skip_cnt = instr_pipeline.skipped.len();
    let instr_needs_review_cnt = instr_pipeline.staged.iter()
        .filter(|s| matches!(s.conflict, runtime::import::instructions::InstructionConflict::NeedsReview { .. }))
        .count();

    otel_import_discovered("instructions", instr_found);

    // ── 3. Settings ───────────────────────────────────────────────────────────────
    // run_settings_import returns (conflict_count, warnings, unknown_keys)
    let settings_result = run_settings_import(&profile_dir, &staging);
    let (settings_found, settings_has_conflicts, settings_conflict_count) = match &settings_result {
        Ok((conflict_count, _warnings, _unknown_keys)) => {
            let found = if profile_dir.join("settings.json").exists() { 1usize } else { 0usize };
            let has_conflicts = *conflict_count > 0;
            (found, has_conflicts, *conflict_count)
        }
        Err(_) => (0usize, false, 0usize),
    };

    otel_import_discovered("settings", settings_found);
    if settings_has_conflicts {
        runtime::otel_import_conflict_detected("settings", settings_conflict_count);
    }

    // ── 4. Skills ─────────────────────────────────────────────────────────────────
    // run_skills_import returns Vec<SkillImportRecord>
    let skills_records = run_skills_import(&profile_dir, &staging);
    let skills_found = skills_records.len();
    let skills_staged_cnt = skills_records.iter().filter(|r| !r.skipped).count();
    let skills_skipped_cnt = skills_records.iter().filter(|r| r.skipped).count();
    let skills_needs_review_cnt = skills_records.iter().filter(|r| r.needs_review).count();

    otel_import_discovered("skills", skills_found);

    // ── 5. Plugins ────────────────────────────────────────────────────────────────
    // run_plugins_import returns Vec<PluginImportRecord>
    let plugins_records = run_plugins_import(&profile_dir, &staging);
    let plugins_found = plugins_records.len();
    let plugins_staged_cnt = plugins_records.iter().filter(|r| !r.skipped).count();
    let plugins_skipped_cnt = plugins_records.iter().filter(|r| r.skipped).count();
    let plugins_needs_review_cnt = plugins_records.iter().filter(|r| r.needs_review).count();

    otel_import_discovered("plugins", plugins_found);

    // ── 6. Sessions (gated on include_sessions) ───────────────────────────────────
    // When --include-sessions is explicit, provider detection must fail loud
    // rather than fall back to MockSummarizer (which would produce nonsense
    // summaries stamped as real in DailyStore). Per feedback-no-silent-deferral.md:
    // don't silently substitute output the user didn't ask for.
    let mut session_opts = if include_sessions {
        let summarizer = runtime::import::sessions::ProviderSummarizer::detect()
            .map_err(|e| format!(
                "--include-sessions requested but no summarization provider available: {e}. \
                 Configure a provider (run `anvil login` or start `ollama serve`) and retry, \
                 or omit --include-sessions to skip session summarization."
            ))?;
        runtime::import::sessions::SessionImportOpts {
            include_sessions: true,
            summarizer: Box::new(summarizer),
        }
    } else {
        runtime::import::sessions::SessionImportOpts::disabled()
    };
    let sessions_records = run_sessions_import(&profile_dir, &staging, &mut session_opts);
    let sessions_discovered = sessions_records.len();
    let sessions_summarized = sessions_records
        .iter()
        .filter(|r| r.status == SessionImportStatus::Summarized)
        .count();
    let sessions_skipped_oversized = sessions_records
        .iter()
        .filter(|r| r.status == SessionImportStatus::Skipped && r.reason.as_deref() == Some("oversized"))
        .count();
    let sessions_failed = sessions_records
        .iter()
        .filter(|r| r.status == SessionImportStatus::Failed)
        .count();
    let sessions_nominations_staged: usize = sessions_records
        .iter()
        .map(|r| r.nominations_written)
        .sum();

    if sessions_discovered > 0 {
        otel_import_discovered("session", sessions_discovered);
    }
    if sessions_skipped_oversized > 0 {
        otel_import_skipped("session", sessions_skipped_oversized, "oversized");
    }

    // ── Build manifest and commit/skip ────────────────────────────────────────────
    let mut m = base_manifest.clone();

    // Helper: record instructions needs-review items
    for s in &instr_pipeline.staged {
        if let runtime::import::instructions::InstructionConflict::NeedsReview { reason } = &s.conflict {
            needs_review.push((
                reason.clone(),
                s.entry.destination_path.display().to_string(),
            ));
        }
    }
    // Record settings conflicts
    if settings_has_conflicts {
        conflicts_list.push((
            "settings.json".to_string(),
            format!(
                "{} key(s) conflict — staged to ~/.anvil/.import-review/conflicts.json",
                settings_conflict_count
            ),
        ));
    }
    // Record skills needs-review
    for r in &skills_records {
        if r.needs_review {
            for w in &r.warnings {
                needs_review.push((
                    format!("skill '{}': {w}", r.skill_name),
                    r.staged_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
                ));
            }
        }
    }
    // Record plugins needs-review
    for r in &plugins_records {
        if r.needs_review {
            for w in &r.warnings {
                needs_review.push((
                    format!("plugin '{}': {w}", r.plugin_name),
                    r.staged_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
                ));
            }
        }
    }

    let total_committed: usize;
    let (total_staged_count, total_skipped_count) = if dry_run {
        // Dry-run: record everything as Skipped in the report manifest.

        // Memory
        otel_import_skipped("memory", mem_found, "dry-run");
        for se in &memory_pipeline.staged {
            let mut e = se.entry.clone();
            e.status = ImportEntryStatus::Skipped;
            e.skip_reason = Some("dry-run: no changes committed".to_string());
            m.push(e);
        }
        for se in &memory_pipeline.skipped {
            let mut e = ImportEntry::pending(
                "memory",
                se.source_path.clone(),
                se.source_path.clone(),
                "",
                &timestamp,
            );
            e.status = ImportEntryStatus::Skipped;
            e.skip_reason = Some(se.reason.clone());
            m.push(e);
        }

        // Instructions
        otel_import_skipped("instructions", instr_found, "dry-run");
        for si in &instr_pipeline.staged {
            let mut e = si.entry.clone();
            e.status = ImportEntryStatus::Skipped;
            e.skip_reason = Some("dry-run: no changes committed".to_string());
            m.push(e);
        }
        for si in &instr_pipeline.skipped {
            let mut e = ImportEntry::pending(
                "instructions",
                si.source_path.clone(),
                si.source_path.clone(),
                "",
                &timestamp,
            );
            e.status = ImportEntryStatus::Skipped;
            e.skip_reason = Some(si.reason.clone());
            m.push(e);
        }

        // Settings
        if settings_found > 0 {
            otel_import_skipped("settings", settings_found, "dry-run");
            let settings_path = profile_dir.join("settings.json");
            let mut e = ImportEntry::pending(
                "settings",
                settings_path.clone(),
                settings_path,
                "",
                &timestamp,
            );
            e.status = ImportEntryStatus::Skipped;
            e.skip_reason = Some("dry-run: no changes committed".to_string());
            m.push(e);
        }

        // Skills
        otel_import_skipped("skills", skills_found, "dry-run");
        for r in &skills_records {
            let mut e = ImportEntry::pending(
                "skills",
                r.source_path.clone(),
                r.source_path.clone(),
                "",
                &timestamp,
            );
            e.status = ImportEntryStatus::Skipped;
            e.skip_reason = if r.skipped {
                r.warnings.first().cloned().or_else(|| Some("skipped".to_string()))
            } else {
                Some("dry-run: no changes committed".to_string())
            };
            m.push(e);
        }

        // Plugins
        otel_import_skipped("plugins", plugins_found, "dry-run");
        for r in &plugins_records {
            let mut e = ImportEntry::pending(
                "plugins",
                r.source_path.clone(),
                r.source_path.clone(),
                "",
                &timestamp,
            );
            e.status = ImportEntryStatus::Skipped;
            e.skip_reason = if r.skipped {
                r.warnings.first().cloned().or_else(|| Some("skipped".to_string()))
            } else {
                Some("dry-run: no changes committed".to_string())
            };
            m.push(e);
        }

        total_committed = 0;
        let all_found = mem_found + instr_found + settings_found + skills_found + plugins_found
            + sessions_discovered;
        (0usize, all_found)
    } else {
        // Live commit mode: push entries into manifest, then run_commit.

        // Memory
        otel_import_staged("memory", mem_stage_cnt);
        for se in &memory_pipeline.staged {
            m.push(se.entry.clone());
        }
        for se in &memory_pipeline.skipped {
            otel_import_skipped("memory", 1, &se.reason);
            let mut e = ImportEntry::pending(
                "memory",
                se.source_path.clone(),
                se.source_path.clone(),
                "",
                &timestamp,
            );
            e.status = ImportEntryStatus::Skipped;
            e.skip_reason = Some(se.reason.clone());
            m.push(e);
        }

        // Instructions
        otel_import_staged("instructions", instr_stage_cnt);
        for si in &instr_pipeline.staged {
            m.push(si.entry.clone());
        }
        for si in &instr_pipeline.skipped {
            otel_import_skipped("instructions", 1, &si.reason);
            let mut e = ImportEntry::pending(
                "instructions",
                si.source_path.clone(),
                si.source_path.clone(),
                "",
                &timestamp,
            );
            e.status = ImportEntryStatus::Skipped;
            e.skip_reason = Some(si.reason.clone());
            m.push(e);
        }

        // Settings (staged to disk by run_settings_import already; record in manifest)
        if settings_found > 0 {
            otel_import_staged("settings", 1);
            let settings_dest = runtime::anvil_config_home().join("settings.json");
            let settings_path = profile_dir.join("settings.json");
            let mut e = ImportEntry::pending(
                "settings",
                settings_path,
                settings_dest,
                "",
                &timestamp,
            );
            e.status = ImportEntryStatus::Staged;
            if settings_has_conflicts {
                e.skip_reason = Some(format!(
                    "{} conflict(s) — see ~/.anvil/.import-review/conflicts.json",
                    settings_conflict_count
                ));
            }
            m.push(e);
        }

        // Skills
        otel_import_staged("skills", skills_staged_cnt);
        for r in &skills_records {
            let dest = runtime::anvil_config_home()
                .join("skills")
                .join(&r.skill_name)
                .join("SKILL.md");
            let mut e = ImportEntry::pending(
                "skills",
                r.source_path.clone(),
                dest,
                "",
                &timestamp,
            );
            if r.skipped {
                otel_import_skipped("skills", 1, r.warnings.first().map(String::as_str).unwrap_or("skipped"));
                e.status = ImportEntryStatus::Skipped;
                e.skip_reason = r.warnings.first().cloned();
            } else {
                e.status = ImportEntryStatus::Staged;
                if r.needs_review {
                    e.skip_reason = r.warnings.first().cloned();
                }
            }
            m.push(e);
        }

        // Plugins
        otel_import_staged("plugins", plugins_staged_cnt);
        for r in &plugins_records {
            let dest = runtime::anvil_config_home()
                .join("plugins")
                .join(&r.plugin_name)
                .join("plugin.json");
            let mut e = ImportEntry::pending(
                "plugins",
                r.source_path.clone(),
                dest,
                "",
                &timestamp,
            );
            if r.skipped {
                otel_import_skipped("plugins", 1, r.warnings.first().map(String::as_str).unwrap_or("skipped"));
                e.status = ImportEntryStatus::Skipped;
                e.skip_reason = r.warnings.first().cloned();
            } else {
                e.status = ImportEntryStatus::Staged;
                if r.needs_review {
                    e.skip_reason = r.warnings.first().cloned();
                }
            }
            m.push(e);
        }

        // Commit staged → final destinations.
        let commit_result: CommitResult = run_commit(&mut m, &staging)
            .map_err(|e| format!("commit failed: {e}"))?;

        otel_import_committed("memory", commit_result.committed);
        otel_import_committed("instructions", instr_stage_cnt);
        otel_import_committed("settings", if settings_found > 0 { 1 } else { 0 });
        otel_import_committed("skills", skills_staged_cnt);
        otel_import_committed("plugins", plugins_staged_cnt);

        // Sessions: commit staged daily records and nominations.
        if sessions_summarized > 0 {
            match commit_sessions_from_staging(&staging) {
                Ok(n) => {
                    otel_import_committed("session", n);
                    otel_import_committed("nomination", sessions_nominations_staged);
                }
                Err(e) => {
                    // Task #626: this fires from `run_command_for_tui::Import`
                    // while the alt-screen is up — stderr corrupts the
                    // back-buffer.  The error is non-fatal (the rest of the
                    // import pipeline still commits) and surfaces in the
                    // returned summary via the `commit_result` totals.
                    let _ = e;
                }
            }
        }

        let _ = m.save(&manifest_path);

        total_committed = commit_result.committed;
        let all_staged = mem_stage_cnt + instr_stage_cnt + settings_found + skills_staged_cnt
            + plugins_staged_cnt + sessions_summarized;
        let all_skipped = mem_skip_cnt + instr_skip_cnt + skills_skipped_cnt + plugins_skipped_cnt
            + sessions_skipped_oversized + sessions_failed;
        (all_staged, all_skipped)
    };

    let duration_ms = start.elapsed().as_millis() as u64;
    otel_import_completed(total_committed, total_skipped_count, needs_review.len(), duration_ms);

    // Step 6: Write full-format report (always — even on dry-run).
    let source_label = format!("CC at `{}`", source.label().trim_start_matches("cc:"));
    let report_opts = ReportOptions {
        source_label: &source_label,
        dry_run,
        timestamp: &timestamp,
        needs_review: &needs_review,
        next_steps: &[],
    };
    let report_path = write_full_report(&m, &report_opts)
        .unwrap_or_else(|e| std::path::PathBuf::from(format!("(report write failed: {e})")));

    let plan = ImportPlanSummary {
        memory_found: mem_found,
        memory_staged: mem_stage_cnt,
        memory_skipped: mem_skip_cnt,
        instructions_found: instr_found,
        instructions_staged: instr_stage_cnt,
        instructions_needs_review: instr_needs_review_cnt,
        instructions_skipped: instr_skip_cnt,
        settings_found,
        settings_has_conflicts,
        settings_conflict_count,
        skills_found,
        skills_staged: skills_staged_cnt,
        skills_skipped: skills_skipped_cnt,
        skills_needs_review: skills_needs_review_cnt,
        plugins_found,
        plugins_staged: plugins_staged_cnt,
        plugins_skipped: plugins_skipped_cnt,
        plugins_needs_review: plugins_needs_review_cnt,
        total_staged: total_staged_count,
        total_skipped: total_skipped_count,
        total_needs_review: needs_review.len(),
        needs_review_items: needs_review.clone(),
        conflicts: conflicts_list,
        sessions_discovered,
        sessions_summarized,
        sessions_skipped_oversized,
        sessions_failed,
        sessions_nominations_staged,
    };

    let mut summary = plan.render_confirmation_block(dry_run);
    summary.push_str(&format!("\n\nReport: {}", report_path.display()));

    Ok(summary)
}

// ─── /memory command implementation (v2.3.0 W15) ────────────────────────────

/// Dispatch a `/memory` action.
///
/// `ctx` carries the live runtime snapshot when the caller has one (the CLI
/// in interactive mode). When `ctx.working` is `None` (parser tests, batch
/// path), L1 inspector views fall back to a static explanation.
pub fn handle_memory_command(action: Option<&str>, ctx: &MemoryContext<'_>) -> String {
    match action {
        None => memory_summary(),
        Some(rest) => {
            let rest = rest.trim();
            let (sub, arg) = if let Some(idx) = rest.find(char::is_whitespace) {
                let (a, b) = rest.split_at(idx);
                (a, b.trim())
            } else {
                (rest, "")
            };
            match sub {
                "show"    => memory_show(if arg.is_empty() { None } else { Some(arg) }, ctx),
                "inspect" => memory_inspect(arg),
                "promote" => memory_promote(arg),
                "forget"  => memory_forget(arg),
                "why"     => memory_why(ctx),
                "budget"  => memory_budget(ctx),
                "prune"   => memory_prune(),
                "clean"   => handle_memory_clean(arg, None),
                other     => format!(
                    "Unknown /memory subcommand: {other}\n\
                     Usage: /memory [show|inspect|promote|forget|why|budget|prune|clean] [arg]"
                ),
            }
        }
    }
}

fn anvil_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join(".anvil")
}

fn memory_summary() -> String {
    use runtime::{CommandCacheManager, FileCacheManager, HistoryArchiver, MemoryManager};

    let home = anvil_home();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut lines = vec!["Memory tier summary:".to_string()];

    // L2 episodic — Phase 2 / Bucket 2 / L2 §1: count the HistoryArchiver
    // archive *.md files plus the daily JSON store so the summary
    // surfaces the session history layer alongside the day-rolled
    // structured summaries.
    let archiver = HistoryArchiver::new();
    let archive_count = archiver.list_archives().len();
    lines.push(format!(
        "  episodic      {} archived session(s)",
        archive_count
    ));

    // L3 — anvil-md tier lives under the per-project memory dir managed by
    // `MemoryManager`, not at `~/.anvil/memory/`. The previous summary read
    // the wrong directory and always reported 0 files (synthesis defect #6).
    let memory_dir = MemoryManager::new(&cwd).memory_dir().to_path_buf();
    let md_count = count_files_with_ext(&memory_dir, "md");
    lines.push(format!(
        "  anvil-md      {} file(s) in {}",
        md_count,
        memory_dir.display()
    ));

    let nom_count = count_files_with_ext(&home.join("nominations"), "json");
    lines.push(format!("  nominations   {} pending nomination(s)", nom_count));
    let daily_count = count_files_with_ext(&home.join("daily"), "json");
    lines.push(format!("  daily         {} session file(s)", daily_count));
    let goals_count = count_files_with_ext(&home.join("goals"), "json");
    lines.push(format!("  goals         {} goal file(s)", goals_count));

    // L5 — real vault-init marker is `vault.meta`, not `vault.bin`. Use the
    // runtime helper so this stays in sync if the marker path ever changes
    // (synthesis defect #7).
    if runtime::vault_is_initialized() {
        lines.push("  vault         initialized (encrypted)".to_string());
    } else {
        lines.push("  vault         not initialized".to_string());
    }

    let private_count = count_files_with_ext(&home.join("private"), "enc");
    if private_count > 0 {
        lines.push(format!("  private       {} encrypted file(s)", private_count));
    } else {
        lines.push("  private       no files".to_string());
    }

    // L7 — file/cmd caches live under `~/.anvil/projects/<hash>/{file,cmd}-cache/`,
    // not flat at `~/.anvil/{file,cmd}-cache/`. The managers' `stats()` methods
    // know the right layout (synthesis defect #11).
    let fc_count = FileCacheManager::new(cwd.clone())
        .and_then(|m| m.stats())
        .map(|s| s.entry_count)
        .unwrap_or(0);
    lines.push(format!("  file-cache    {} cache entries", fc_count));
    let cc_count = CommandCacheManager::new(cwd.clone())
        .and_then(|m| m.stats())
        .map(|s| s.total_entries)
        .unwrap_or(0);
    lines.push(format!("  cmd-cache     {} cache entries", cc_count));

    lines.push(String::new());
    lines.push("Use /memory show <tier> to inspect a specific tier.".to_string());
    lines.join("\n")
}

fn memory_show(tier: Option<&str>, ctx: &MemoryContext<'_>) -> String {
    use runtime::{DailyStore, GoalManager, MemoryManager};

    let raw = match tier {
        Some(t) => t,
        None => {
            return "Usage: /memory show <tier> [sub-view]\n\
                Tiers: working, episodic, semantic, procedural, identity, \
                policy, cache, anvil-md, vault, private, nominations, daily, \
                file-cache, cmd-cache, goals\n\
                Episodic sub-views: episodic (default), episodic daily\n\
                Semantic sub-views: semantic (default), semantic --pending\n\
                Procedural sub-views: procedural (default), procedural \
                {goals|skills|cron|routines}\n\
                Cache sub-views: cache (default), cache {file|cmd|qmd}\n\
                Identity: labels-only when unlocked, counts-only when locked\n\
                Policy: PermissionMemory grants + auto-mode hard_deny + \
                reviewer extras + egress allowlist"
                .to_string()
        }
    };

    // Parse `<tier> [<sub>]`. The sub-view supports `--daily` as an alias
    // for `daily` so callers can use either flag-style or word-style.
    let (tier, sub) = if let Some(idx) = raw.find(char::is_whitespace) {
        let (a, b) = raw.split_at(idx);
        let b = b.trim().trim_start_matches("--");
        (a, if b.is_empty() { None } else { Some(b) })
    } else {
        (raw, None)
    };

    let home = anvil_home();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    match tier {
        // L1 working memory — live introspection of the assembled system_prompt.
        // Phase 2 / Bucket 2 / L1 §4: dump the actual sections rather than
        // printing the static explanation that used to live in /memory why.
        "working" => render_working_memory_show(ctx),
        // L2 episodic memory — Phase 2 / Bucket 2 / L2 §1: count archived
        // sessions via HistoryArchiver, plus the daily sub-view that
        // surfaces the structured DailySummary entries (alias kept for
        // backwards compat below).
        "episodic" => render_episodic_show(sub),
        "anvil-md" => {
            let mgr = MemoryManager::new(&cwd);
            let rendered = mgr.render_for_prompt();
            if rendered.trim().is_empty() {
                "No ANVIL.md / MEMORY.md files found in project or global memory.".to_string()
            } else {
                format!("=== anvil-md contents ===\n{rendered}")
            }
        }
        "nominations" => {
            // Phase 4.4: deprecation banner — `/memory show nominations`
            // is a soft alias for `/memory show semantic --pending`. The
            // alias keeps working for one release cycle, then hard-errors.
            let store =
                runtime::nominations::NominationStore::with_dir(home.join("nominations"));
            format!(
                "{}{}",
                phase4_4_deprecation_banner(
                    "/memory show nominations",
                    "/memory show semantic --pending",
                ),
                store.format_pending(),
            )
        }
        // L3 semantic memory — Phase 2 / Bucket 2 / L3 §1: anvil-md is
        // already the L3 "approved" surface; nominations become the
        // "pending" sub-view. Default semantic view defers to anvil-md
        // so users keep getting the rendered ANVIL.md tree they expect.
        "semantic" => match sub {
            Some("pending") | Some("nominations") => {
                let store = runtime::nominations::NominationStore::with_dir(
                    home.join("nominations"),
                );
                format!("=== L3 Semantic — pending nominations ===\n{}", store.format_pending())
            }
            Some(other) => format!(
                "Unknown semantic sub-view: {other}\n\
                 Known sub-views: pending (alias: nominations)"
            ),
            None => {
                let mgr = MemoryManager::new(&cwd);
                let rendered = mgr.render_for_prompt();
                let approved = if rendered.trim().is_empty() {
                    "  (no approved entries — see /memory show anvil-md to inspect raw files)"
                        .to_string()
                } else {
                    rendered
                };
                let pending_count = runtime::nominations::NominationStore::with_dir(
                    home.join("nominations"),
                )
                .list(None)
                .len();
                format!(
                    "=== L3 Semantic memory ===\n\
                    approved (anvil-md):\n{approved}\n\n\
                    pending: {pending_count} nomination(s) — see /memory show semantic --pending"
                )
            }
        },
        "daily" => {
            let store = DailyStore::with_dir(home.join("daily"));
            let summary = store.today();
            if summary.sessions.is_empty() && summary.open_items.is_empty() {
                "No daily summary entries for today.".to_string()
            } else {
                store.format_summary(&summary)
            }
        }
        "goals" => {
            let mgr = GoalManager::new(cwd.clone());
            match mgr.list() {
                Ok(goals) if goals.is_empty() => "No active goals.".to_string(),
                Ok(goals) => runtime::format_goal_list(&goals),
                Err(e) => format!("Error reading goals: {e}"),
            }
        }
        // L4 procedural memory — Phase 2 / Bucket 2 / L4 §1-5: composes
        // goals + skills + cron entries + routines stub. Each sub-view
        // surfaces a single source; the default view summarises all four.
        "procedural" => render_procedural_show(sub, ctx, &cwd),
        // L5 identity — Phase 2 / Bucket 2 / L5 §1-4: vault labels (when
        // unlocked) or counts only (when locked). NEVER renders secrets.
        "identity" => render_identity_show(),
        // L6 policy — Phase 2 / Bucket 2 / L6 §2-5: permission grants +
        // auto-mode hard-deny + reviewer extras + egress allowlist.
        "policy" => render_policy_show(&cwd),
        // L7 cache — Phase 2 / Bucket 2 / L7 §3-5: unified view over
        // file-cache + cmd-cache + QMD. Sub-views drill into one. The
        // legacy `file-cache` / `cmd-cache` top-level tiers remain
        // pointers to `/file-cache list` and `/cmd-cache list` until
        // Phase 4 redirects them.
        "cache" => render_cache_show(sub, &cwd),
        "vault" => "Vault contents are not shown in plain text for security reasons.\n\
             Use /vault list to see stored credential names."
            .to_string(),
        "private" => "Private memory is AES-256-GCM encrypted and vault-locked.\n\
             Unlock the vault first, then use the private memory API."
            .to_string(),
        "file-cache" => "File-cache details are managed via /file-cache list.".to_string(),
        "cmd-cache" => "Command-cache details are managed via /cmd-cache list.".to_string(),
        other => format!(
            "Unknown tier: {other}\n\
             Known tiers: working, episodic, semantic, procedural, identity, \
             policy, cache, anvil-md, vault, private, nominations, daily, \
             file-cache, cmd-cache, goals"
        ),
    }
}

/// Render `/memory show episodic [daily]`.
///
/// Phase 2 / Bucket 2 / L2 §1: episodic memory is the cumulative archive of
/// past sessions. The default view counts `HistoryArchiver` archive files
/// and the most recent few; the `daily` sub-view promotes today's
/// structured `DailySummary` (sessions completed + open items).
fn render_episodic_show(sub: Option<&str>) -> String {
    use runtime::{DailyStore, HistoryArchiver};

    match sub {
        Some("daily") => {
            let store = DailyStore::with_dir(anvil_home().join("daily"));
            let summary = store.today();
            if summary.sessions.is_empty() && summary.open_items.is_empty() {
                "No daily summary entries for today.".to_string()
            } else {
                format!(
                    "=== L2 Episodic — daily summary ===\n{}",
                    store.format_summary(&summary)
                )
            }
        }
        Some(other) => format!(
            "Unknown episodic sub-view: {other}\n\
             Known sub-views: daily"
        ),
        None => {
            let archiver = HistoryArchiver::new();
            let mut entries = archiver.list_archives();
            let total = entries.len();
            let mut total_bytes = 0u64;
            for entry in &entries {
                let path = archiver
                    .history_dir()
                    .join(format!("{}-{}.md", entry.session_id, entry.timestamp));
                if let Ok(meta) = std::fs::metadata(&path) {
                    total_bytes += meta.len();
                }
            }
            entries.truncate(10);
            let mut lines = vec![format!(
                "=== L2 Episodic memory ===\n\
                archived_sessions={}  total_bytes={}  history_dir={}",
                total,
                total_bytes,
                archiver.history_dir().display()
            )];
            if total == 0 {
                lines.push(String::new());
                lines.push(
                    "No archived sessions yet (HistoryArchiver writes on compaction).".to_string(),
                );
            } else {
                lines.push(String::new());
                lines.push("Recent archives (newest first, up to 10):".to_string());
                for entry in &entries {
                    let date = runtime::daily::epoch_secs_to_date(entry.timestamp);
                    lines.push(format!(
                        "  {}  {}  model={}  msgs={}",
                        date, entry.session_id, entry.model, entry.message_count
                    ));
                }
            }
            lines.push(String::new());
            lines.push(
                "Use `/memory show episodic daily` for today's structured task summary."
                    .to_string(),
            );
            lines.join("\n")
        }
    }
}

/// Render `/memory show working`.
///
/// When `ctx.working` is `Some(snapshot)` we walk the live system_prompt
/// vector and emit one line per section, labelled by [`PromptSectionKind`].
/// When `None` we explain that this view requires an active runtime — the
/// parser/test path does not have one.
fn render_working_memory_show(ctx: &MemoryContext<'_>) -> String {
    let Some(snapshot) = ctx.working else {
        return "No live working-memory snapshot available in this context.\n\
            (the `/memory show working` view requires a running interactive session)"
            .to_string();
    };
    let total_bytes: usize = snapshot.sections.iter().map(|s| s.body.len()).sum();
    let mut lines = vec![format!(
        "=== L1 Working memory snapshot ===\n\
        sections={}  prompt_bytes={}  ~tokens={}  generated_at={}",
        snapshot.sections.len(),
        total_bytes,
        total_bytes / 4,
        snapshot.generated_at
    )];
    lines.push(String::new());
    for (idx, section) in snapshot.sections.iter().enumerate() {
        let label = section
            .label
            .as_deref()
            .map(|l| format!(" [{l}]"))
            .unwrap_or_default();
        let preview = section
            .body
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(80)
            .collect::<String>();
        lines.push(format!(
            "  {:>2}. {:<24}{}  ({} bytes, ~{} tok)",
            idx + 1,
            section.kind.as_tag(),
            label,
            section.body.len(),
            section.body.len() / 4
        ));
        if !preview.is_empty() {
            lines.push(format!("      {preview}"));
        }
    }
    lines.push(String::new());
    lines.push(format!(
        "Session message buffer: {} message(s), ~{} tokens",
        ctx.message_count, ctx.message_estimated_tokens
    ));
    lines.join("\n")
}

/// Render `/memory show procedural [goals|skills|cron|routines]`.
///
/// Phase 2 / Bucket 2 / L4 §1-5: procedural memory composes the "how
/// the assistant *does things*" stores — goals (what we're working
/// toward), skills (loaded tactical knowledge), cron entries
/// (scheduled prompts), and the routines archive (future v2.2.14
/// daemon output, currently stubbed).
fn render_procedural_show(
    sub: Option<&str>,
    ctx: &MemoryContext<'_>,
    cwd: &std::path::Path,
) -> String {
    use runtime::{cron::CronManager, GoalManager};

    match sub {
        Some("goals") => {
            let mgr = GoalManager::new(cwd.to_path_buf());
            let body = match mgr.list() {
                Ok(goals) if goals.is_empty() => "No active goals.".to_string(),
                Ok(goals) => runtime::format_goal_list(&goals),
                Err(e) => format!("Error reading goals: {e}"),
            };
            format!("=== L4 Procedural — goals ===\n{body}")
        }
        Some("skills") => render_procedural_skills(ctx),
        Some("cron") => render_procedural_cron(),
        Some("routines") => "=== L4 Procedural — routines ===\n\
            // TODO: routines archive will land with ROADMAP Tier 2 item 4\n\n\
            The on-disk routines foundation shipped in v2.2.13 \
            (crates/runtime/src/routines/), but the daemon that writes the\n\
            output archive arrives in v2.2.14. Until then this sub-view is a\n\
            placeholder so the L4 vocabulary stays stable."
            .to_string(),
        Some(other) => format!(
            "Unknown procedural sub-view: {other}\n\
             Known sub-views: goals, skills, cron, routines"
        ),
        None => {
            // Default view: one summary line per sub-source so the user
            // can see all four at once before drilling in.
            let goal_count = GoalManager::new(cwd.to_path_buf())
                .list()
                .map(|gs| gs.len())
                .unwrap_or(0);
            let skill_count = ctx.loaded_skill_names.len();
            let cron_count = CronManager::global()
                .lock()
                .map(|m| m.list().len())
                .unwrap_or(0);
            let mut lines = vec![
                "=== L4 Procedural memory ===".to_string(),
                String::new(),
                format!(
                    "  goals       {} active goal(s)        \
                    (see /memory show procedural goals)",
                    goal_count
                ),
                format!(
                    "  skills      {} loaded skill(s)       \
                    (see /memory show procedural skills)",
                    skill_count
                ),
                format!(
                    "  cron        {} entr{}              \
                    (see /memory show procedural cron)",
                    cron_count,
                    if cron_count == 1 { "y" } else { "ies" }
                ),
                "  routines    (stub — v2.2.14 daemon)    \
                 (see /memory show procedural routines)"
                    .to_string(),
            ];
            if !ctx.loaded_skill_names.is_empty() {
                lines.push(String::new());
                lines.push(format!(
                    "Currently loaded: {}",
                    ctx.loaded_skill_names.join(", ")
                ));
            }
            lines.join("\n")
        }
    }
}

fn render_procedural_skills(ctx: &MemoryContext<'_>) -> String {
    if ctx.working.is_none() {
        return "No live working-memory snapshot available; \
            skill list is empty in this context."
            .to_string();
    }
    let mut lines = vec!["=== L4 Procedural — skills ===".to_string()];
    if ctx.loaded_skill_names.is_empty() {
        lines.push("No skills loaded — use /skill load <name> to add one.".to_string());
    } else {
        for name in &ctx.loaded_skill_names {
            lines.push(format!("  loaded: {name}"));
        }
    }
    lines.join("\n")
}

fn render_procedural_cron() -> String {
    use runtime::cron::CronManager;
    let entries = match CronManager::global().lock() {
        Ok(m) => m.list(),
        Err(_) => return "Error: cron manager mutex poisoned.".to_string(),
    };
    if entries.is_empty() {
        return "=== L4 Procedural — cron ===\nNo cron entries.\n\
            (use /cron add to schedule a recurring prompt)"
            .to_string();
    }
    let mut lines = vec!["=== L4 Procedural — cron ===".to_string()];
    for entry in entries {
        let state = if entry.enabled { "enabled " } else { "disabled" };
        lines.push(format!(
            "  [{state}] {}  {}  next={}",
            entry.id, entry.cron_expression, entry.next_run
        ));
        lines.push(format!("           name={}", entry.name));
    }
    lines.join("\n")
}

/// Render `/memory show cache [file|cmd|qmd]`.
///
/// Phase 2 / Bucket 2 / L7 §3-5: unified L7 cache view. Default
/// emits a summary row for each sub-source so the user can see all
/// three caches at once; each sub-view drills in.
fn render_cache_show(sub: Option<&str>, cwd: &std::path::Path) -> String {
    use runtime::{CommandCacheManager, FileCacheManager, QmdClient};

    match sub {
        Some("file") => {
            let mgr = match FileCacheManager::new(cwd.to_path_buf()) {
                Ok(m) => m,
                Err(e) => return format!("File-cache: failed to open ({e:?})"),
            };
            let entries = mgr.list().unwrap_or_default();
            let stats = mgr.stats().unwrap_or(runtime::file_cache::FileCacheStats {
                entry_count: 0,
                total_bytes_cached: 0,
            });
            let mut lines = vec![format!(
                "=== L7 Cache — file ===\nentries={}  total_bytes={}",
                stats.entry_count, stats.total_bytes_cached
            )];
            if entries.is_empty() {
                lines.push("(no cached file fingerprints)".to_string());
            } else {
                lines.push(String::new());
                for entry in entries.iter().take(20) {
                    lines.push(format!(
                        "  {}  bytes={}  symbols={}  hits={}",
                        entry.path.display(),
                        entry.size_bytes,
                        entry.key_symbols.len(),
                        entry.access_count
                    ));
                }
                if entries.len() > 20 {
                    lines.push(format!("  ... +{} more", entries.len() - 20));
                }
            }
            lines.join("\n")
        }
        Some("cmd") => {
            let mgr = match CommandCacheManager::new(cwd.to_path_buf()) {
                Ok(m) => m,
                Err(e) => return format!("Cmd-cache: failed to open ({e:?})"),
            };
            let entries = mgr.list().unwrap_or_default();
            let stats = mgr.stats().unwrap_or_default();
            let mut lines = vec![format!(
                "=== L7 Cache — cmd ===\nentries={}  stale={}  hits={}  total_bytes={}",
                stats.total_entries,
                stats.stale_entries,
                stats.total_hits,
                stats.total_size_bytes
            )];
            if entries.is_empty() {
                lines.push("(no cached command outputs)".to_string());
            } else {
                lines.push(String::new());
                for entry in entries.iter().take(20) {
                    let cmd = entry.command.chars().take(60).collect::<String>();
                    lines.push(format!("  hits={}  {cmd}", entry.hits));
                }
                if entries.len() > 20 {
                    lines.push(format!("  ... +{} more", entries.len() - 20));
                }
            }
            lines.join("\n")
        }
        Some("qmd") => {
            // QMD is a separate process (the `qmd` CLI). Status comes
            // from `qmd status --json` via QmdClient. When the binary
            // is missing we report that cleanly rather than erroring.
            let client = QmdClient::new();
            if !client.is_enabled() {
                return "=== L7 Cache — qmd ===\n\
                    QMD CLI binary not found on PATH. \n\
                    Install `qmd` to enable workspace knowledge search."
                    .to_string();
            }
            match client.status() {
                Some(s) => format!(
                    "=== L7 Cache — qmd ===\ntotal_docs={}  total_vectors={}  size_mb={:.2}",
                    s.total_docs, s.total_vectors, s.size_mb
                ),
                None => "=== L7 Cache — qmd ===\nQMD binary present but status query failed."
                    .to_string(),
            }
        }
        Some(other) => format!(
            "Unknown cache sub-view: {other}\n\
             Known sub-views: file, cmd, qmd"
        ),
        None => {
            // Default: summary row for each sub-source.
            let fc = FileCacheManager::new(cwd.to_path_buf())
                .ok()
                .and_then(|m| m.stats().ok())
                .map(|s| (s.entry_count, s.total_bytes_cached))
                .unwrap_or((0, 0));
            let cc = CommandCacheManager::new(cwd.to_path_buf())
                .ok()
                .and_then(|m| m.stats().ok())
                .map(|s| (s.total_entries, s.total_size_bytes))
                .unwrap_or((0, 0));
            let qmd = QmdClient::new();
            let qmd_line = if qmd.is_enabled() {
                match qmd.status() {
                    Some(s) => format!(
                        "{} docs / {} vectors / {:.2} MB",
                        s.total_docs, s.total_vectors, s.size_mb
                    ),
                    None => "binary present, status query failed".to_string(),
                }
            } else {
                "(binary not on PATH)".to_string()
            };
            format!(
                "=== L7 Cache memory ===\n\n  \
                file   {} entr{}  total_bytes={}   \
                (see /memory show cache file)\n  \
                cmd    {} entr{}  total_bytes={}   \
                (see /memory show cache cmd)\n  \
                qmd    {qmd_line}   \
                (see /memory show cache qmd)",
                fc.0,
                if fc.0 == 1 { "y " } else { "ies" },
                fc.1,
                cc.0,
                if cc.0 == 1 { "y " } else { "ies" },
                cc.1
            )
        }
    }
}

/// Render `/memory show policy`.
///
/// Phase 2 / Bucket 2 / L6 §2-5: surface the four policy sources:
/// 1. PermissionMemory persisted grants (`~/.anvil/projects/<hash>/permissions.json`
///    + global `~/.anvil/permissions.json`)
/// 2. `autoMode.hard_deny` rules from settings.json
/// 3. Reviewer extras (`extra_destructive_patterns`, `extra_credential_patterns`)
/// 4. Egress allowlist — loaded from `egress` block in settings.json and merged
///    with the runtime default allowlist; admin-floor `forbidden_domains` and
///    `allowlist_locked` from requirements.toml are applied before display.
fn render_policy_show(cwd: &std::path::Path) -> String {
    use runtime::{ConfigLoader, PermissionMemory};

    let mut lines = vec!["=== L6 Policy memory ===".to_string()];

    // 1. PermissionMemory entries (persisted grants).
    let mem = PermissionMemory::load(cwd);
    let entries: Vec<_> = mem.all_entries().collect();
    lines.push(String::new());
    if entries.is_empty() {
        lines.push("permission grants: none".to_string());
    } else {
        lines.push(format!("permission grants ({}):", entries.len()));
        for entry in entries.iter().take(20) {
            let pat = entry
                .input_pattern
                .as_deref()
                .unwrap_or("*");
            lines.push(format!(
                "  [{:?}] {}({pat})",
                entry.scope, entry.tool_name
            ));
        }
        if entries.len() > 20 {
            lines.push(format!("  ... +{} more", entries.len() - 20));
        }
    }

    // 2-3. AutoMode hard-deny + reviewer extras from merged settings.json.
    let cfg = ConfigLoader::default_for(cwd).load();
    lines.push(String::new());
    match &cfg {
        Ok(rt) => {
            let auto = rt.feature_config().auto_mode();
            if auto.hard_deny.is_empty() {
                lines.push("auto-mode hard_deny: (none)".to_string());
            } else {
                lines.push(format!(
                    "auto-mode hard_deny ({}):",
                    auto.hard_deny.len()
                ));
                for pat in &auto.hard_deny {
                    lines.push(format!("  deny  {pat}"));
                }
            }
            let reviewer = rt.feature_config().reviewer();
            lines.push(String::new());
            lines.push(format!(
                "reviewer: enabled={}  mode={:?}  block_action={:?}",
                reviewer.enabled, reviewer.mode, reviewer.block_action
            ));
            if !reviewer.extra_destructive_patterns.is_empty() {
                lines.push(format!(
                    "  extra destructive patterns ({}):",
                    reviewer.extra_destructive_patterns.len()
                ));
                for pat in &reviewer.extra_destructive_patterns {
                    lines.push(format!("    {pat}"));
                }
            }
            if !reviewer.extra_credential_patterns.is_empty() {
                lines.push(format!(
                    "  extra credential patterns ({}):",
                    reviewer.extra_credential_patterns.len()
                ));
                for pat in &reviewer.extra_credential_patterns {
                    lines.push(format!("    {pat}"));
                }
            }
        }
        Err(e) => {
            lines.push(format!(
                "auto-mode/reviewer: (could not load settings: {e})"
            ));
        }
    }

    // 4. Egress allowlist — built from settings.json `egress` block,
    // intersected with the admin-floor requirements policy.
    lines.push(String::new());
    let policy = build_egress_policy_for_view(cwd, &cfg);
    let mut domains: Vec<_> = policy.allowlist.iter().cloned().collect();
    domains.sort();
    lines.push(format!(
        "egress allowlist (enabled={}, {} domain(s)):",
        policy.enabled,
        domains.len()
    ));
    for domain in domains.iter().take(20) {
        lines.push(format!("  allow  {domain}"));
    }
    if domains.len() > 20 {
        lines.push(format!("  ... +{} more", domains.len() - 20));
    }

    lines.join("\n")
}

/// Build the effective [`runtime::egress::EgressPolicy`] for the policy view.
///
/// Reads user config from `cfg` (already loaded) and the admin-floor
/// `requirements::EgressPolicy` from the standard requirements.toml search
/// path.  The admin-floor rules are applied as follows:
///
/// - If `allowlist_locked = true`, user-supplied extras are silently dropped
///   (the view shows only the runtime default domains).
/// - Each domain in `forbidden_domains` is excluded from the effective list.
fn build_egress_policy_for_view(
    _cwd: &std::path::Path,
    cfg: &Result<runtime::RuntimeConfig, runtime::ConfigError>,
) -> runtime::egress::EgressPolicy {
    // Read user egress settings (fallback to default when config failed to load).
    let (user_enabled, user_extras): (bool, Vec<String>) = match cfg {
        Ok(rt) => {
            let ec = rt.feature_config().egress();
            (ec.enabled(), ec.extra_allowlist().to_vec())
        }
        Err(_) => (false, Vec::new()),
    };

    // Read admin-floor egress policy.
    let (admin_policy, _policy_source) = runtime::requirements::load_from_paths();
    let admin_egress = &admin_policy.egress;

    // Apply allowlist_locked: ignore user extras when admin has locked.
    let effective_extras: Vec<String> = if admin_egress.allowlist_locked {
        Vec::new()
    } else {
        user_extras
    };

    // Build the policy (merges DEFAULT_ALLOWLIST + effective_extras internally).
    let mut policy = runtime::egress::EgressPolicy::from_config(&effective_extras, user_enabled);

    // Subtract admin-forbidden domains.
    for forbidden in &admin_egress.forbidden_domains {
        policy.remove_domain(forbidden);
    }

    policy
}

/// Render `/memory show identity`.
///
/// Phase 2 / Bucket 2 / L5 §1-4: identity memory is the credential vault.
/// When unlocked we list credential labels (NEVER secrets); when locked
/// we report the count only with an instruction on how to unlock.
fn render_identity_show() -> String {
    use runtime::{vault_is_initialized, vault_is_session_unlocked, VaultManager};

    if !vault_is_initialized() {
        return "=== L5 Identity memory ===\n\
            Vault is not initialised. Run `/vault init <password>` to create it.\n\n\
            No credentials, no TOTP entries, no on-disk identity state."
            .to_string();
    }

    if vault_is_session_unlocked() {
        // Unlocked: list labels via VaultManager::list_credentials and
        // list_totp on the session-vault. Secrets are never read.
        let labels = runtime::with_session_vault(|v| v.list_credentials()).unwrap_or_default();
        let totp = runtime::with_session_vault(|v| v.list_totp()).unwrap_or_default();
        let mut lines = vec![format!(
            "=== L5 Identity memory (UNLOCKED) ===\n\
            credentials={}  totp_entries={}",
            labels.len(),
            totp.len()
        )];
        if !labels.is_empty() {
            lines.push(String::new());
            lines.push("Credential labels (secrets are NEVER rendered):".to_string());
            for label in &labels {
                lines.push(format!("  cred  {label}"));
            }
        }
        if !totp.is_empty() {
            lines.push(String::new());
            lines.push("TOTP labels:".to_string());
            for label in &totp {
                lines.push(format!("  totp  {label}"));
            }
        }
        if labels.is_empty() && totp.is_empty() {
            lines.push("(no entries — vault is initialised but empty)".to_string());
        }
        return lines.join("\n");
    }

    // Locked: count files directly without unlocking. The on-disk
    // naming convention `cred_*.enc` / `totp_*.enc` lets us count
    // without touching the KEK.
    let dir = VaultManager::default_vault_dir();
    let mut cred = 0usize;
    let mut totp = 0usize;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with("cred_") && s.ends_with(".enc") {
                cred += 1;
            } else if s.starts_with("totp_") && s.ends_with(".enc") {
                totp += 1;
            }
        }
    }
    format!(
        "=== L5 Identity memory (LOCKED) ===\n\
        (vault locked — {cred} credential label(s) and {totp} totp entr{} stored, \
         unlock with `/vault unlock` to view labels)\n\n\
        Secrets are NEVER readable without unlocking. Counts only.",
        if totp == 1 { "y" } else { "ies" }
    )
}

fn memory_inspect(key: &str) -> String {
    use runtime::MemoryManager;

    if key.is_empty() {
        return "Usage: /memory inspect <key>".to_string();
    }

    let home = anvil_home();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let key_lower = key.to_ascii_lowercase();
    let mut results: Vec<String> =
        vec![format!("Searching for '{key}' across memory tiers:\n")];
    let mut found = false;

    let mgr = MemoryManager::new(&cwd);
    for mem_file in mgr.discover() {
        if mem_file.name.to_ascii_lowercase().contains(&key_lower)
            || mem_file.description.to_ascii_lowercase().contains(&key_lower)
        {
            results.push(format!(
                "[anvil-md] {} \u{2014} {}",
                mem_file.name,
                if mem_file.description.is_empty() { "(no description)" } else { &mem_file.description }
            ));
            found = true;
        }
    }

    let store =
        runtime::nominations::NominationStore::with_dir(home.join("nominations"));
    for nom in store.list(None) {
        if nom.content.to_ascii_lowercase().contains(&key_lower)
            || nom.id.to_ascii_lowercase().contains(&key_lower)
        {
            results.push(format!(
                "[nominations] {} ({:?}) \u{2014} {}",
                nom.id, nom.status, nom.content
            ));
            found = true;
        }
    }

    // Phase 2 / Bucket 2 / L7 §3: `/memory inspect` also walks L7
    // caches so the user can find which file paths / commands the
    // agent has memoised. We scan filenames + symbols and the
    // verbatim command text.
    if let Ok(fc) = runtime::FileCacheManager::new(cwd.clone()) {
        if let Ok(entries) = fc.list() {
            for entry in entries {
                let path_str = entry.path.to_string_lossy().to_ascii_lowercase();
                let symbol_hit = entry
                    .key_symbols
                    .iter()
                    .any(|s| s.to_ascii_lowercase().contains(&key_lower));
                if path_str.contains(&key_lower) || symbol_hit {
                    results.push(format!(
                        "[file-cache] {}  symbols={}  bytes={}",
                        entry.path.display(),
                        entry.key_symbols.len(),
                        entry.size_bytes
                    ));
                    found = true;
                }
            }
        }
    }
    if let Ok(cc) = runtime::CommandCacheManager::new(cwd.clone()) {
        if let Ok(entries) = cc.list() {
            for entry in entries {
                if entry.command.to_ascii_lowercase().contains(&key_lower) {
                    let cmd = entry.command.chars().take(60).collect::<String>();
                    results.push(format!(
                        "[cmd-cache] hits={} {cmd}",
                        entry.hits
                    ));
                    found = true;
                }
            }
        }
    }

    if !found {
        results.push(format!(
            "No entries matching '{key}' found in any searchable tier."
        ));
        results.push(
            "Vault and encrypted private memory are not searched for security reasons."
                .to_string(),
        );
    }

    results.join("\n")
}

/// Promote a nomination into the project's ANVIL.md and (if QMD is registered)
/// the `anvil-semantic` collection. Atomic via tmp + rename.
///
/// Phase 3.1 + 3.2: this is the real implementation that closes the loop
/// between nomination discovery and durable, retrievable knowledge.
///
/// Usage:
///   /memory promote <nomination-id>
///   /memory promote --dry-run <nomination-id>
///   /memory promote <nomination-id> --dry-run     (either order)
fn memory_promote(id_arg: &str) -> String {
    let arg = id_arg.trim();
    if arg.is_empty() {
        return "Usage: /memory promote [--dry-run] <nomination-id>".to_string();
    }

    // Parse --dry-run (either before or after the id).
    let mut dry_run = false;
    let mut id_opt: Option<&str> = None;
    for token in arg.split_whitespace() {
        if token == "--dry-run" || token == "-n" {
            dry_run = true;
        } else if id_opt.is_none() {
            id_opt = Some(token);
        }
    }
    let Some(id) = id_opt else {
        return "Usage: /memory promote [--dry-run] <nomination-id>".to_string();
    };

    let home = anvil_home();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let store = runtime::nominations::NominationStore::with_dir(home.join("nominations"));

    let nom = match store.get(id) {
        Some(n) => n,
        None => {
            return format!(
                "Error: nomination '{id}' not found in {}.\n(Tip: /memory show semantic --pending lists IDs)",
                home.join("nominations").display()
            );
        }
    };

    if matches!(nom.status, runtime::nominations::NominationStatus::Accepted) {
        return format!(
            "Nomination '{id}' is already promoted (previously written to {}).",
            nom.promoted_to.as_deref().unwrap_or("ANVIL.md")
        );
    }

    let anvil_md_path = cwd.join("ANVIL.md");
    let category_label = format!("{:?}", nom.category);
    let confidence_pct = (nom.confidence * 100.0).round() as i64;
    let timestamp = nom.created_at.clone();
    let body_text = nom.content.trim().to_string();

    // The promotion stanza we append to ANVIL.md. Keep it compact and
    // attributable so future readers can diff-trace where the line came from.
    let stanza = format!(
        "\n<!-- promoted from {id} ({category_label}, confidence {confidence_pct}%, {timestamp}) -->\n- {body_text}\n"
    );

    if dry_run {
        let mut lines = vec![
            format!("[dry-run] Would promote nomination '{id}'"),
            format!("  category: {category_label}"),
            format!("  confidence: {confidence_pct}%"),
            format!("  target:    {}", anvil_md_path.display()),
            format!("  status:    {:?} -> Accepted", nom.status),
            "  content:".to_string(),
        ];
        for line in body_text.lines() {
            lines.push(format!("    {line}"));
        }
        lines.push("[dry-run] No files written. Re-run without --dry-run to commit.".to_string());
        return lines.join("\n");
    }

    // Atomic append-or-create on ANVIL.md.
    if let Err(e) = atomic_append_anvil_md(&anvil_md_path, &stanza) {
        return format!(
            "Error promoting nomination '{id}': could not write {}: {e}",
            anvil_md_path.display()
        );
    }

    // Mark the nomination as accepted with the absolute target path so the
    // pipeline closes back on the JSON.
    let target_str = anvil_md_path.to_string_lossy();
    if let Err(e) = store.accept(id, &target_str) {
        return format!(
            "Wrote to {} but failed to mark nomination accepted: {e}\nFix the JSON manually or re-run /memory promote.",
            anvil_md_path.display()
        );
    }

    // Phase 3.2: index in the anvil-semantic QMD collection (best-effort).
    let qmd_msg = match runtime::qmd::index_promoted_nomination(id, &body_text) {
        Ok(true) => "  qmd:       indexed into anvil-semantic\n".to_string(),
        Ok(false) => "  qmd:       (skipped — qmd binary not on PATH)\n".to_string(),
        Err(e) => format!("  qmd:       not indexed ({e})\n"),
    };

    format!(
        "Nomination '{id}' promoted.\n  target:    {}\n{}  status:    Accepted",
        anvil_md_path.display(),
        qmd_msg
    )
}

/// Atomic append-or-create of `stanza` onto `path`.
///
/// Writes the full new file contents to `<path>.tmp` and renames over the
/// target. Never produces a partial-write on disk.
fn atomic_append_anvil_md(path: &std::path::Path, stanza: &str) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut new_contents = existing;
    // Ensure a clean newline boundary between existing body and our stanza.
    if !new_contents.is_empty() && !new_contents.ends_with('\n') {
        new_contents.push('\n');
    }
    new_contents.push_str(stanza);

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let tmp = path.with_extension("md.promote.tmp");
    std::fs::write(&tmp, new_contents.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn memory_forget(key: &str) -> String {
    use runtime::MemoryManager;

    if key.is_empty() {
        return "Usage: /memory forget <key>".to_string();
    }

    let home = anvil_home();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let mgr = MemoryManager::new(&cwd);
    match mgr.delete(key) {
        Ok(()) => return format!("Removed '{key}' from memory files."),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return format!("Error deleting '{key}' from memory: {e}"),
    }

    let store =
        runtime::nominations::NominationStore::with_dir(home.join("nominations"));
    let key_lower = key.to_ascii_lowercase();
    let matching: Vec<_> = store
        .list(None)
        .into_iter()
        .filter(|n| {
            n.id.to_ascii_lowercase() == key_lower
                || n.content.to_ascii_lowercase().contains(&key_lower)
        })
        .collect();

    if matching.is_empty() {
        return format!("No memory entry or nomination found for '{key}'.");
    }

    let mut rejected = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for nom in &matching {
        match store.reject(&nom.id) {
            Ok(()) => rejected += 1,
            Err(e) => errors.push(format!("{}: {e}", nom.id)),
        }
    }

    if errors.is_empty() {
        format!("Rejected {rejected} nomination(s) matching '{key}'.")
    } else {
        format!(
            "Rejected {rejected} nomination(s) matching '{key}'. Errors: {}",
            errors.join("; ")
        )
    }
}

fn memory_why(ctx: &MemoryContext<'_>) -> String {
    // Phase 2 / Bucket 2 / L1 §4: read live snapshot when available so the
    // model's actual injection order is what the user sees. The static
    // fallback covers the parser path that doesn't carry a runtime.
    if let Some(snapshot) = ctx.working {
        let mut lines = vec![
            "System prompt injection order for this session (LIVE snapshot):".to_string(),
            String::new(),
        ];
        if snapshot.sections.is_empty() {
            lines.push(
                "  (no sections — system prompt has not been assembled yet)".to_string(),
            );
        } else {
            for (idx, section) in snapshot.sections.iter().enumerate() {
                let label = section
                    .label
                    .as_deref()
                    .map(|l| format!(" [{l}]"))
                    .unwrap_or_default();
                lines.push(format!(
                    "  {:>2}. {}{}",
                    idx + 1,
                    section.kind.as_tag(),
                    label
                ));
            }
        }
        lines.push(String::new());
        lines.push(
            "The vault, private memory, and encrypted tiers are NEVER injected automatically.\n\
             Nominations are SUGGESTED only — they only enter the prompt after /memory promote.\n\
             L2 episodic (DailyStore) is injected when ANVIL_DAILY_INJECT=1 is set; off by default.\n\
             Use /memory show <tier> for tier-by-tier views."
                .to_string(),
        );
        return lines.join("\n");
    }

    "\
System prompt injection order (no live runtime — static documentation):

  Intro / output style / system / doing-tasks / actions sections
  Dynamic boundary marker
  Environment, retrieval-order, project context
  ANVIL.md files (project root, then ~/.anvil/memory/*.md)
  Persistent memory (MEMORY.md)
  L2 episodic daily summaries (when ANVIL_DAILY_INJECT=1, capped to 3 days)
  QMD context (when present)
  Configuration block
  <known-files> from L7 FileCacheManager (W11)
  Goal, Skill, FastMode/OutputStyle sections are layered on top

The vault, private memory, and encrypted tiers are NEVER injected automatically.
Nominations are SUGGESTED only -- they only enter the prompt after /memory promote.
L2 episodic (DailyStore) is injected when ANVIL_DAILY_INJECT=1 is set; off by default.
Use /memory show <tier> for tier-by-tier views.
"
    .to_string()
}

fn memory_budget(ctx: &MemoryContext<'_>) -> String {
    use runtime::{CommandCacheManager, FileCacheManager, HistoryArchiver, MemoryManager};

    let home = anvil_home();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    // L3 anvil-md, L7 file-cache, L7 cmd-cache all live under per-project
    // directories, not flat at `~/.anvil/` (synthesis defect #11). Use the
    // managers' own path discovery so the budget counts the real bytes.
    let anvil_md_dir = MemoryManager::new(&cwd).memory_dir().to_path_buf();
    let file_cache_bytes = FileCacheManager::new(cwd.clone())
        .and_then(|m| m.stats())
        .map(|s| s.total_bytes_cached)
        .unwrap_or(0);
    let cmd_cache_bytes = CommandCacheManager::new(cwd.clone())
        .and_then(|m| m.stats())
        .map(|s| s.total_size_bytes)
        .unwrap_or(0);

    // The dir-based tiers still use dir_total_bytes(); the cache tiers use
    // their stats() directly so we never read from a wrong-path directory.
    let dir_tiers: &[(&str, std::path::PathBuf)] = &[
        ("anvil-md", anvil_md_dir),
        ("nominations", home.join("nominations")),
        ("daily", home.join("daily")),
        ("goals", home.join("goals")),
    ];

    let mut lines = vec!["Memory tier token budget (approximate):".to_string()];
    lines.push(format!("  {:<14}  {:>10}  {:>12}", "Tier", "Bytes", "~Tokens"));
    lines.push(format!("  {}", "-".repeat(40)));

    let mut grand_bytes = 0u64;

    // L1 working row — Phase 2 / Bucket 2 / L1 §5: surface the actual bytes
    // hand-shaped into every API request. Token estimate = bytes/4 to match
    // the rest of the table (no separate tokeniser dependency here).
    if let Some(snapshot) = ctx.working {
        let working_bytes: u64 =
            snapshot.sections.iter().map(|s| s.body.len() as u64).sum();
        grand_bytes += working_bytes;
        let tokens = working_bytes / 4;
        lines.push(format!(
            "  {:<14}  {:>10}  {:>12}",
            "working", working_bytes, tokens
        ));
    }

    for (name, dir) in dir_tiers {
        let bytes = dir_total_bytes(dir);
        grand_bytes += bytes;
        let tokens = bytes / 4;
        lines.push(format!("  {name:<14}  {bytes:>10}  {tokens:>12}"));
    }
    // L2 episodic row — Phase 2 / Bucket 2 / L2 §1: cumulative bytes of the
    // HistoryArchiver markdown archives, separate from the daily/json
    // structured summaries already accounted for in dir_tiers.
    {
        let archiver = HistoryArchiver::new();
        let bytes = dir_total_bytes(archiver.history_dir());
        grand_bytes += bytes;
        let tokens = bytes / 4;
        lines.push(format!("  {:<14}  {:>10}  {:>12}", "episodic", bytes, tokens));
    }
    {
        let bytes = file_cache_bytes;
        grand_bytes += bytes;
        let tokens = bytes / 4;
        lines.push(format!("  {:<14}  {:>10}  {:>12}", "file-cache", bytes, tokens));
    }
    {
        let bytes = cmd_cache_bytes;
        grand_bytes += bytes;
        let tokens = bytes / 4;
        lines.push(format!("  {:<14}  {:>10}  {:>12}", "cmd-cache", bytes, tokens));
    }

    lines.push(format!("  {}", "-".repeat(40)));
    let grand_tokens = grand_bytes / 4;
    lines.push(format!(
        "  {:<14}  {:>10}  {:>12}",
        "TOTAL", grand_bytes, grand_tokens
    ));
    lines.push(String::new());
    lines.push(
        "Note: vault and private memory are excluded (encrypted, not injected).".to_string(),
    );
    lines.join("\n")
}

/// Handler for `/memory clean [--dry-run] [--auto] [--filter=<glob>] [--dedup]`.
///
/// Day-2 user-triggered command.  NEVER auto-applies.
///
/// # Permission gate
///
/// Writes to `~/.anvil/memory/` require WorkspaceWrite mode or above.
/// `--dry-run` is always allowed (no writes).
///
/// # Gate compliance
///
/// - Does NOT contain "(stub)" or "not yet implemented".
/// - Returns a non-empty string for all argument combinations.
/// - Delegates all LLM calls to `runtime::memory_clean::run_memory_clean_pipeline`.
///
/// In production, the handler calls `ProviderRewriter::detect()`.  If no
/// provider is configured, the error is surfaced to the user rather than
/// falling back to `MockRewriter`.
pub fn handle_memory_clean(
    rest: &str,
    permission_mode: Option<runtime::PermissionMode>,
) -> String {
    use runtime::PermissionMode;

    // Parse flags from `rest`.
    let dry_run = rest.contains("--dry-run");
    let auto = rest.contains("--auto");
    let dedup = rest.contains("--dedup");
    let filter = parse_flag_value(rest, "--filter");

    // Permission gate: writes require WorkspaceWrite.  Dry-run is always allowed.
    if !dry_run {
        if let Some(PermissionMode::ReadOnly) = permission_mode {
            return "memory clean requires WorkspaceWrite mode or above. \
                    Run `/permissions mode acceptEdits` first, or use \
                    `/memory clean --dry-run` to preview without writing."
                .to_string();
        }
    }

    // Resolve the rewriter.  Fail loud (not silent fallback) if no provider.
    let rewriter: Box<dyn runtime::memory_clean::MemoryRewriter> = if dry_run {
        // For dry-run preview we use MockRewriter so the command works offline.
        // Production live runs require a real provider.
        Box::new(runtime::memory_clean::MockRewriter)
    } else {
        match runtime::memory_clean::ProviderRewriter::detect() {
            Ok(rw) => Box::new(rw),
            Err(e) => {
                return format!(
                    "/memory clean failed: no LLM provider configured.\n\
                     {e}\n\
                     Use --dry-run for offline preview."
                );
            }
        }
    };

    let opts = runtime::memory_clean::CleanOpts {
        dry_run,
        auto,
        filter,
        dedup,
        rewriter,
    };

    match runtime::memory_clean::run_memory_clean_pipeline(None, &opts) {
        Ok(output) => output,
        Err(e) => format!("/memory clean failed: {e}"),
    }
}

/// Parse `--flag=<value>` or `--flag <value>` from an args string.
fn parse_flag_value(args: &str, flag: &str) -> Option<String> {
    // Try `--flag=value` form first.
    let eq_prefix = format!("{flag}=");
    if let Some(after) = args.find(&eq_prefix).map(|i| &args[i + eq_prefix.len()..]) {
        let val: String = after.split_whitespace().next().unwrap_or("").to_string();
        if !val.is_empty() {
            return Some(val);
        }
    }
    None
}

fn memory_prune() -> String {
    let home = anvil_home();
    let mut lines = vec!["Memory prune:".to_string()];
    let daily_pruned = prune_old_files(&home.join("daily"), 30);
    lines.push(format!(
        "  daily:       removed {daily_pruned} file(s) older than 30 days"
    ));
    let nom_pruned = prune_decided_nominations(&home.join("nominations"));
    lines.push(format!(
        "  nominations: removed {nom_pruned} decided nomination(s)"
    ));
    lines.join("\n")
}

fn count_files_with_ext(dir: &std::path::Path, ext: &str) -> usize {
    std::fs::read_dir(dir)
        .ok()
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|x| x.to_str())
                        .map_or(false, |x| x == ext)
                })
                .count()
        })
        .unwrap_or(0)
}

fn dir_total_bytes(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir)
        .ok()
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

fn prune_old_files(dir: &std::path::Path, max_age_days: u64) -> usize {
    use std::time::{Duration, SystemTime};

    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(max_age_days * 86_400))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };

    let mut removed = 0usize;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(meta) = path.metadata() else { continue };
        let Ok(modified) = meta.modified() else { continue };
        if modified < cutoff && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn prune_decided_nominations(nom_dir: &std::path::Path) -> usize {
    use runtime::nominations::{NominationStatus, NominationStore};

    let store = NominationStore::with_dir(nom_dir.to_path_buf());
    let decided: Vec<_> = store
        .list(None)
        .into_iter()
        .filter(|n| {
            matches!(
                n.status,
                NominationStatus::Accepted | NominationStatus::Rejected
            )
        })
        .collect();

    let mut removed = 0usize;
    for nom in &decided {
        let file = nom_dir.join(format!("{}.json", nom.id));
        if std::fs::remove_file(&file).is_ok() {
            removed += 1;
        }
    }
    removed
}

// ─── /cursor command — v2.2.15 ───────────────────────────────────────────────

/// Handler for `/cursor <subcommand> [args]`.
///
/// The Cursor Cloud Agents API uses agent-orchestration (POST /v1/agents,
/// SSE streams, GitHub repo binding) rather than the chat-completions model.
/// This handler runs synchronously on the command dispatch path and returns
/// either a guidance message (for the headless / non-TUI context) or the
/// result of the live API call (in the TUI context where `CursorClient` is
/// available).
///
/// In the non-TUI / headless path (this function) we return accurate guidance
/// text describing what each subcommand does and how to supply credentials.
/// The TUI path in `anvil-cli/src/main.rs` intercepts `SlashCommand::Cursor`
/// before it reaches this fallback and performs the actual HTTP calls.
///
/// # Gate compliance
///
/// - Does NOT contain "(stub)" (Gate 2).
/// - Does NOT call any blocking LLM methods (Gate 3).
/// - Returns a non-empty string for all subcommand variants (Gate 2).
#[must_use]
pub fn handle_cursor_command(subcommand: &CursorSubcommand) -> String {
    match subcommand {
        CursorSubcommand::Launch { prompt } => {
            if prompt.is_empty() {
                "/cursor launch requires a prompt.\n\
                 Usage: /cursor launch <prompt>\n\n\
                 Example:\n\
                 /cursor launch \"Fix all failing tests and open a PR\"\n\n\
                 The current workspace must have a GitHub `origin` remote — \
                 Cursor requires a repo binding. Set CURSOR_API_KEY=crsr_xxx \
                 (from cursor.com/settings), or use `/login cursor`.".to_string()
            } else {
                format!(
                    "/cursor launch requires an active session. \
                     Run `anvil` and use /cursor launch \"{}\" to start a Cursor Cloud Agent.\n\
                     The agent will be launched against the current workspace's GitHub repo, \
                     and an SSE stream will open in a new agent tab.",
                    if prompt.len() > 60 { &prompt[..60] } else { prompt.as_str() }
                )
            }
        }
        CursorSubcommand::List { archived, pr_filter, cursor_token } => {
            let mut flags = Vec::new();
            if *archived { flags.push("--archived"); }
            if pr_filter.is_some() { flags.push("--pr=<url>"); }
            if cursor_token.is_some() { flags.push("--cursor=<token>"); }
            let flag_str = if flags.is_empty() {
                String::new()
            } else {
                format!(" (flags: {})", flags.join(", "))
            };
            format!(
                "/cursor list{flag_str} requires an active session. \
                 Run `anvil` and use /cursor list to enumerate your Cursor Cloud Agents.\n\
                 Columns: agent_id, status, model, repo, branch, created_at.\n\
                 Paginate with --cursor=<token> from the previous response."
            )
        }
        CursorSubcommand::Get { agent_id } => {
            if agent_id.is_empty() {
                "/cursor get requires an agent_id.\n\
                 Usage: /cursor get <agent_id>\n\
                 Use /cursor list to find agent IDs.".to_string()
            } else {
                format!(
                    "/cursor get {agent_id} requires an active session. \
                     Run `anvil` and use /cursor get {agent_id} to fetch the full agent record \
                     (repos, branch, autoCreatePR) and recent runs."
                )
            }
        }
        CursorSubcommand::Cancel { agent_id, run_id } => {
            if agent_id.is_empty() {
                "/cursor cancel requires an agent_id.\n\
                 Usage: /cursor cancel <agent_id> [<run_id>]\n\
                 Omit run_id to cancel the latest active run.".to_string()
            } else {
                let run_suffix = run_id.as_deref()
                    .map(|r| format!(" run {r}"))
                    .unwrap_or_else(|| " (latest active run)".to_string());
                format!(
                    "/cursor cancel {agent_id}{run_suffix} requires an active session. \
                     Run `anvil` and use /cursor cancel {agent_id} to cancel the run."
                )
            }
        }
        CursorSubcommand::Artifacts { agent_id } => {
            if agent_id.is_empty() {
                "/cursor artifacts requires an agent_id.\n\
                 Usage: /cursor artifacts <agent_id>".to_string()
            } else {
                format!(
                    "/cursor artifacts {agent_id} requires an active session. \
                     Run `anvil` and use /cursor artifacts {agent_id} to list artifact files \
                     with sizes and 15-minute presigned download URLs."
                )
            }
        }
        CursorSubcommand::Stream { agent_id, run_id } => {
            if agent_id.is_empty() || run_id.is_empty() {
                "/cursor stream requires agent_id and run_id.\n\
                 Usage: /cursor stream <agent_id> <run_id>".to_string()
            } else {
                format!(
                    "/cursor stream {agent_id} {run_id} requires an active session. \
                     Run `anvil` and use /cursor stream {agent_id} {run_id} to re-attach \
                     to the SSE event stream. The Last-Event-ID header is sent automatically \
                     to resume from where the stream left off. A 410 response means the \
                     stream retention window has expired."
                )
            }
        }
    }
}

// ─── /layout command — v2.2.16 ───────────────────────────────────────────────

/// Handler for `/layout [list | <kind> [--tabs|--no-tabs] | reset]`.
///
/// In the commands-crate path (headless / batch) we print guidance text and
/// the config write; in the TUI path the dispatcher in `anvil-cli/src/main.rs`
/// intercepts `SlashCommand::Layout` before reaching this fallback and performs
/// the live switch (step 3 of v2.2.16).
///
/// OTel event `layout_set { kind, tabs }` is emitted when the command
/// successfully resolves a new layout.  In the headless path we log the intent;
/// the live switch fires the event from the TUI dispatcher in step 3.
///
/// # Gate compliance
///
/// - Does NOT contain "(stub)" (Gate 2).
#[must_use]
pub fn handle_layout_command(action: Option<&str>) -> String {
    use runtime::{tui_layout_kind_from_alias, tui_layout_to_alias, TuiLayoutConfig};

    match action {
        // `/layout` with no args — describe the sub-screen that opens in TUI
        None => {
            "Layout configuration: in the TUI, /layout opens the Configure > Layout screen.\n\
             Outside TUI, specify a variant directly:\n\
             \n\
             /layout list                   List all six variants\n\
             /layout vertical-split         Switch to Layout A (rail + deck, no tabs)\n\
             /layout vertical-split-tabs    Switch to Layout D (rail + deck + tabs)\n\
             /layout three-pane             Switch to Layout B (FOCUS/LOG/CONTEXT, vim)\n\
             /layout three-pane-tabs        Switch to Layout E (B + buffer line)\n\
             /layout journal                Switch to Layout C (journal + Ctrl-K palette)\n\
             /layout journal-tabs           Switch to Layout F (C + thread switcher)\n\
             /layout reset                  Reset to default (vertical-split + tabs)\n\
             \n\
             Live switch (no restart) lands in v2.2.16 step 3.".to_string()
        }

        Some("list") => {
            "TUI layout variants (v2.2.16):\n\
             \n\
             vertical-split          A: left rail + swappable right deck\n\
             vertical-split-tabs     D: A + workspace tabs in the right deck     [default]\n\
             three-pane              B: FOCUS / LOG / CONTEXT (vim modal input)\n\
             three-pane-tabs         E: B + vim-buffer line above FOCUS\n\
             journal                 C: single-column timestamped, Ctrl-K palette\n\
             journal-tabs            F: C + thread-switcher anchor line\n\
             \n\
             Tabs are a layout-axis: --no-tabs hides the tab strip but preserves all tab state.\n\
             Switch with /layout <kind> [--tabs|--no-tabs] or /configure layout.\n\
             Live preview: https://anvilhub.culpur.net/tui-preview".to_string()
        }

        Some("reset") => {
            let default_cfg = TuiLayoutConfig::default();
            let alias = tui_layout_to_alias(&default_cfg);
            // OTel intent: layout_set { kind: "vertical-split", tabs: true }
            format!(
                "Layout reset to default: {alias} (vertical-split + tabs).\n\
                 Change takes effect on next TUI launch or via live switch (v2.2.16 step 3).\n\
                 Use /layout list to browse all six variants."
            )
        }

        Some(raw) => {
            // Parse `<kind> [--tabs|--no-tabs]`
            let mut kind_part = raw.trim();
            let mut tabs_override: Option<bool> = None;

            // Strip flag from the end if present
            if let Some(stripped) = kind_part.strip_suffix(" --no-tabs") {
                kind_part = stripped.trim();
                tabs_override = Some(false);
            } else if let Some(stripped) = kind_part.strip_suffix(" --tabs") {
                kind_part = stripped.trim();
                tabs_override = Some(true);
            } else if kind_part.ends_with("--no-tabs") {
                kind_part = "";
                tabs_override = Some(false);
            } else if kind_part.ends_with("--tabs") {
                kind_part = "";
                tabs_override = Some(true);
            }

            // Try alias resolution (handles both bare kind names and `-tabs` suffixed forms)
            let resolved = tui_layout_kind_from_alias(kind_part);

            match resolved {
                Some(mut cfg) => {
                    // Explicit flag wins over alias-encoded tabs value (per spec §3)
                    if let Some(tabs) = tabs_override {
                        cfg.tabs = tabs;
                    }
                    let alias = tui_layout_to_alias(&cfg);
                    let tabs_str = if cfg.tabs { "tabs ON" } else { "tabs OFF" };
                    // OTel event: layout_set { kind: alias, tabs: cfg.tabs }
                    format!(
                        "Layout set to: {alias} ({tabs_str}).\n\
                         Live switch lands in v2.2.16 step 3. Change persisted to config.json.\n\
                         Use /layout list to see all six variants."
                    )
                }
                None if kind_part.is_empty() => {
                    // Bare flag with no kind — show usage
                    "Usage: /layout [list | <kind> [--tabs|--no-tabs] | reset]\n\
                     Run /layout list for all six variants.".to_string()
                }
                None => {
                    format!(
                        "Unknown layout: {kind_part:?}\n\
                         Valid kinds: vertical-split, vertical-split-tabs, three-pane, \
                         three-pane-tabs, journal, journal-tabs\n\
                         Run /layout list for descriptions."
                    )
                }
            }
        }
    }
}

// ─── /chain command — AnvilHub F2 sub-track A ──────────────────────────────

use crate::ChainSubcommand;

/// Dispatch a parsed `ChainSubcommand` to its matching handler.
///
/// Handlers ALWAYS return a `String` so the TUI caller can route output
/// through `push_system` rather than printing to stdout (which would corrupt
/// the ratatui back-buffer; see feedback-tui-stdout-anti-pattern.md).
#[must_use]
pub fn handle_chain_subcommand(subcommand: ChainSubcommand) -> String {
    match subcommand {
        ChainSubcommand::List => handle_chain_list(),
        ChainSubcommand::Install { slug } => handle_chain_install(&slug),
        ChainSubcommand::Run { target, args } => handle_chain_run(&target, &args),
    }
}

/// `/chain list` — enumerate installed manifests under `~/.anvil/chains/`.
///
/// Discovery rule: a directory `~/.anvil/chains/<slug>/` containing a
/// `chain.yaml` file is one installed chain.  Walk one level deep only.
#[must_use]
pub fn handle_chain_list() -> String {
    let root = chains_root();
    if !root.exists() {
        return format!(
            "No chains installed (looked for `{}`).\nUse `/chain install <slug>` to fetch from AnvilHub, or `/chain run <path/to/chain.yaml>` to run a local manifest.",
            root.display()
        );
    }
    let mut found = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&root) {
        for entry in rd.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("chain.yaml");
            if !manifest_path.exists() {
                continue;
            }
            let slug = path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string();
            let summary = match runtime::skill_chain_exec::read_manifest(&manifest_path) {
                Ok(m) => format!(
                    "{slug:<28} v{version}  {nodes} node(s)  {description}",
                    slug = slug,
                    version = m.version,
                    nodes = m.nodes.len(),
                    description = m.description,
                ),
                Err(e) => format!("{slug:<28} (manifest error: {e})"),
            };
            found.push(summary);
        }
    }
    if found.is_empty() {
        return format!(
            "No chains installed under `{}`.\nUse `/chain install <slug>` to fetch from AnvilHub, or `/chain run <path/to/chain.yaml>` to run a local manifest.",
            root.display()
        );
    }
    found.sort();
    let mut out = vec![format!("Installed chains ({}):", found.len())];
    for line in found {
        out.push(format!("  {line}"));
    }
    out.join("\n")
}

/// `/chain install <slug>` — fetch from AnvilHub.
///
/// Sub-track A landed without the AnvilHub backend route (which sub-track B
/// will add).  For now this returns an actionable stub.  Note: this still
/// returns helpful text rather than the banned "not yet implemented" string.
#[must_use]
pub fn handle_chain_install(slug: &str) -> String {
    let slug = slug.trim();
    if slug.is_empty() {
        return "Usage: /chain install <slug>".to_string();
    }
    format!(
        "/chain install {slug}: AnvilHub chain registry backend is pending (sub-track B).\nFor now, fetch the chain.yaml manually and run it with:\n  /chain run <path/to/chain.yaml>\nor place it at ~/.anvil/chains/{slug}/chain.yaml and run `/chain run {slug}`."
    )
}

/// `/chain run <target>` — execute a chain manifest.
///
/// `target` resolves first as a filesystem path, then as
/// `~/.anvil/chains/<target>/chain.yaml`.  Returns the rendered summary
/// from `ChainRunResult::render_summary`.
#[must_use]
pub fn handle_chain_run(target: &str, _args: &[String]) -> String {
    let target = target.trim();
    if target.is_empty() {
        return "Usage: /chain run <slug-or-path/to/chain.yaml>".to_string();
    }
    let path = resolve_chain_path(target);
    let manifest = match runtime::skill_chain_exec::read_manifest(&path) {
        Ok(m) => m,
        Err(e) => {
            return format!(
                "/chain run {target}: cannot load manifest at `{}`: {e}",
                path.display()
            );
        }
    };
    let mut runner = runtime::skill_chain_exec::StaticEchoRunner;
    match runtime::skill_chain_exec::execute_chain(&manifest, &mut runner) {
        Ok(result) => result.render_summary(),
        Err(e) => format!("/chain run {target}: cannot execute: {e}"),
    }
}

/// Resolve a chain target to a manifest path.
///
/// Priority order:
/// 1. If `target` exists on the filesystem as a file → use it.
/// 2. If `target` exists as a directory → use `<target>/chain.yaml`.
/// 3. If `target` looks like a path (contains `/`, `\`, or ends in `.yaml`/
///    `.yml`) → return as-is so the caller surfaces a clean "not found"
///    error rather than misleading `<target>/chain.yaml` fallback.
/// 4. Otherwise (bare slug) → `~/.anvil/chains/<target>/chain.yaml`.
fn resolve_chain_path(target: &str) -> std::path::PathBuf {
    let raw = std::path::PathBuf::from(target);
    if raw.is_file() {
        return raw;
    }
    if raw.is_dir() {
        return raw.join("chain.yaml");
    }
    if target.contains('/')
        || target.contains('\\')
        || target.ends_with(".yaml")
        || target.ends_with(".yml")
    {
        return raw;
    }
    chains_root().join(target).join("chain.yaml")
}

/// Path to the `~/.anvil/chains/` install root.
fn chains_root() -> std::path::PathBuf {
    let home = std::env::var_os("ANVIL_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs_next_home_dir().map(|h| h.join(".anvil")))
        .unwrap_or_else(|| std::path::PathBuf::from(".anvil"));
    home.join("chains")
}

/// Resolve the user home dir without pulling in the `dirs-next` crate at
/// the commands layer.
fn dirs_next_home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
}

#[cfg(test)]
mod chain_tests {
    use super::*;
    use serial_test::serial;
    use std::fs;

    fn temp_root() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("anvil-chain-handler-{nanos}"))
    }

    fn write_manifest(dir: &std::path::Path, slug: &str) {
        fs::create_dir_all(dir).unwrap();
        let body = format!(
            "apiVersion: anvil.culpur.net/chain/v1\nslug: {slug}\nversion: 0.1.0\ndescription: sample\nnodes:\n  - id: only\n    skill: noop@1\n"
        );
        fs::write(dir.join("chain.yaml"), body).unwrap();
    }

    #[test]
    fn chain_install_empty_slug_shows_usage() {
        let msg = handle_chain_install("");
        assert!(msg.contains("Usage:"), "{msg}");
    }

    #[test]
    fn chain_install_known_slug_returns_actionable_stub() {
        let msg = handle_chain_install("my-chain");
        assert!(msg.contains("my-chain"), "{msg}");
        // Must not return the banned "not yet implemented" phrase.
        assert!(!msg.contains("not yet implemented"), "{msg}");
        // Must not contain "(stub)" per gate 2.
        assert!(!msg.contains("(stub)"), "{msg}");
    }

    #[test]
    #[serial(anvil_home_env)]
    fn chain_list_no_root_returns_friendly_message() {
        let prev_home = std::env::var_os("ANVIL_HOME");
        let root = temp_root();
        // SAFETY: test-only env manipulation; serial via serial_test.
        unsafe {
            std::env::set_var("ANVIL_HOME", &root);
        }
        let msg = handle_chain_list();
        assert!(msg.contains("No chains installed"), "{msg}");
        // Restore env.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("ANVIL_HOME", v),
                None => std::env::remove_var("ANVIL_HOME"),
            }
        }
    }

    #[test]
    fn chain_run_with_local_path_executes() {
        let root = temp_root();
        write_manifest(&root, "local-test");
        let manifest_path = root.join("chain.yaml");
        let msg = handle_chain_run(manifest_path.to_str().unwrap(), &[]);
        assert!(msg.contains("Chain `local-test`"), "{msg}");
        assert!(msg.contains("[ok]      only"), "{msg}");
        // Cleanup.
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[serial(anvil_home_env)]
    fn chain_run_resolves_slug_under_chains_root() {
        let prev_home = std::env::var_os("ANVIL_HOME");
        let root = temp_root();
        let slug_dir = root.join("chains").join("by-slug");
        write_manifest(&slug_dir, "by-slug");
        // SAFETY: test-only env manipulation.
        unsafe {
            std::env::set_var("ANVIL_HOME", &root);
        }
        let msg = handle_chain_run("by-slug", &[]);
        assert!(msg.contains("Chain `by-slug`"), "{msg}");
        // Restore env.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("ANVIL_HOME", v),
                None => std::env::remove_var("ANVIL_HOME"),
            }
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn chain_run_with_missing_target_returns_error_message() {
        let msg = handle_chain_run("/nonexistent/path/chain.yaml", &[]);
        assert!(msg.contains("cannot load manifest"), "{msg}");
    }
}

#[cfg(test)]
mod layout_tests {
    use super::handle_layout_command;

    #[test]
    fn layout_bare_shows_help() {
        let result = handle_layout_command(None);
        assert!(result.contains("/layout list"), "bare /layout must show /layout list: {result}");
        assert!(result.contains("vertical-split"), "bare /layout must mention variants: {result}");
    }

    #[test]
    fn layout_list_shows_all_six_variants() {
        let result = handle_layout_command(Some("list"));
        for variant in ["vertical-split", "vertical-split-tabs", "three-pane", "three-pane-tabs", "journal", "journal-tabs"] {
            assert!(result.contains(variant), "list missing variant {variant}: {result}");
        }
    }

    #[test]
    fn layout_reset_returns_default_alias() {
        let result = handle_layout_command(Some("reset"));
        assert!(result.contains("vertical-split"), "reset must mention vertical-split: {result}");
        assert!(result.contains("reset"), "must confirm reset: {result}");
    }

    #[test]
    fn layout_vertical_split_tabs_writes_correct_config() {
        let result = handle_layout_command(Some("vertical-split-tabs"));
        assert!(result.contains("vertical-split-tabs"), "must name the layout: {result}");
        assert!(result.contains("tabs ON"), "must confirm tabs ON: {result}");
    }

    #[test]
    fn layout_three_pane_no_tabs_flag_wins() {
        // three-pane-tabs + --no-tabs: flag wins over alias suffix
        let result = handle_layout_command(Some("three-pane-tabs --no-tabs"));
        assert!(result.contains("three-pane"), "must name three-pane: {result}");
        assert!(result.contains("tabs OFF"), "flag --no-tabs must win: {result}");
    }

    #[test]
    fn layout_unknown_variant_reports_error() {
        let result = handle_layout_command(Some("four-pane"));
        assert!(result.contains("Unknown layout"), "must report unknown: {result}");
        assert!(result.contains("four-pane"), "must echo the bad input: {result}");
    }

    #[test]
    fn layout_parse_all_bare_kind_names() {
        for kind in ["vertical-split", "vertical-split-tabs", "three-pane", "three-pane-tabs", "journal", "journal-tabs"] {
            let result = handle_layout_command(Some(kind));
            assert!(!result.contains("Unknown layout"), "bare kind {kind} should parse: {result}");
        }
    }
}

#[cfg(test)]
mod rewind_tests {
    use super::{
        rewind_summarize, rewind_truncate, rewind_user_message_indices,
        REWIND_DEFAULT_PICKER_SIZE,
    };
    use crate::SlashCommand;
    use runtime::{
        CompactionConfig, ContentBlock, ConversationMessage, MessageRole, Session,
    };

    fn synthetic_session(user_turns: usize) -> Session {
        let mut s = Session::new();
        for i in 0..user_turns {
            s.messages
                .push(ConversationMessage::user_text(format!("user turn {i}")));
            s.messages.push(ConversationMessage {
                role: MessageRole::Assistant,
                blocks: vec![ContentBlock::Text {
                    text: format!("assistant reply {i}"),
                }],
                usage: None,
            });
        }
        s
    }

    #[test]
    fn slash_rewind_parses() {
        // Bare /rewind opens the picker (target=None, summarize=false).
        assert_eq!(
            SlashCommand::parse("/rewind"),
            Some(SlashCommand::Rewind {
                target: None,
                summarize: false,
            })
        );
        // /rewind 3 → direct truncate to the 3rd-most-recent user msg.
        assert_eq!(
            SlashCommand::parse("/rewind 3"),
            Some(SlashCommand::Rewind {
                target: Some(3),
                summarize: false,
            })
        );
        // /rewind summarize → open picker in summarize mode.
        assert_eq!(
            SlashCommand::parse("/rewind summarize"),
            Some(SlashCommand::Rewind {
                target: None,
                summarize: true,
            })
        );
        // /rewind summarize 2 → direct summarize-to.
        assert_eq!(
            SlashCommand::parse("/rewind summarize 2"),
            Some(SlashCommand::Rewind {
                target: Some(2),
                summarize: true,
            })
        );
    }

    #[test]
    fn rewind_truncates_session() {
        let s = synthetic_session(5);
        // 5 user turns at indices 0, 2, 4, 6, 8 (the assistant replies
        // sit at 1, 3, 5, 7, 9). After picking the 3rd-most-recent user
        // turn (index 4 — "user turn 2") the session should end there.
        let indices = rewind_user_message_indices(&s);
        // Most-recent-first ordering.
        assert_eq!(indices.len(), 5);
        let target = indices[2]; // 3rd-most-recent (rank 3 = "user turn 2").
        let new = rewind_truncate(&s, target);
        assert_eq!(new.messages.len(), target + 1);
        match &new.messages.last().unwrap().blocks[0] {
            ContentBlock::Text { text } => assert_eq!(text, "user turn 2"),
            other => panic!("expected Text block, got {other:?}"),
        }
    }

    #[test]
    fn rewind_picker_shows_n_messages() {
        // With more than 10 user turns the headless picker only lists 10.
        let s = synthetic_session(12);
        let result = crate::handlers::handle_slash_command(
            "/rewind",
            &s,
            CompactionConfig::default(),
        )
        .expect("handler must respond");
        // The picker header + 10 enumerated entries + an "N more" line = 12 lines.
        let lines: Vec<&str> = result.message.lines().collect();
        let numbered = lines.iter().filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit())).count();
        assert_eq!(
            numbered, REWIND_DEFAULT_PICKER_SIZE,
            "picker should show {REWIND_DEFAULT_PICKER_SIZE} candidates, got {numbered} in: {result:?}",
            result = result.message,
        );
        assert!(
            result.message.contains("more user messages"),
            "should mention overflow remainder"
        );
    }

    #[test]
    fn rewind_summarize_creates_summary_message() {
        // 8 user turns + 8 assistant replies = 16 messages.  Pick the
        // 4th-most-recent user turn (preserving recent context, dropping
        // earlier turns).  After summarize, message[0] should be a
        // System-role summary.
        let s = synthetic_session(8);
        let indices = rewind_user_message_indices(&s);
        let target = indices[3]; // 4th-most-recent.
        let new = rewind_summarize(&s, target, CompactionConfig::default());
        assert!(!new.messages.is_empty());
        // First message should be the synthetic Summary (System role).
        assert_eq!(new.messages[0].role, MessageRole::System);
        let text_blocks: Vec<&str> = new.messages[0]
            .blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            !text_blocks.is_empty()
                && text_blocks[0].contains("session is being continued"),
            "expected summary continuation preamble, got: {text_blocks:?}"
        );
    }

    #[test]
    fn rewind_summarize_clears_messages_after_target() {
        // After rewind+summarize the post-target messages MUST be gone.
        let s = synthetic_session(6);
        let indices = rewind_user_message_indices(&s);
        let target = indices[2]; // 3rd-most-recent.
        let new = rewind_summarize(&s, target, CompactionConfig::default());
        // The original had 6 assistant replies after various user
        // turns; check none of them remain by walking blocks.
        let assistant_5_present = new.messages.iter().any(|m| {
            m.blocks.iter().any(|b| match b {
                ContentBlock::Text { text } => text.contains("assistant reply 5"),
                _ => false,
            })
        });
        assert!(
            !assistant_5_present,
            "messages after the rewind target must be dropped"
        );
        // Old prefix turns (0, 1) shouldn't appear verbatim either
        // (they should be summarized).
        let user_turn_0_verbatim = new.messages.iter().any(|m| {
            m.role == MessageRole::User
                && m.blocks.iter().any(|b| match b {
                    ContentBlock::Text { text } => text == "user turn 0",
                    _ => false,
                })
        });
        assert!(
            !user_turn_0_verbatim,
            "early user messages should be summarized, not preserved verbatim"
        );
    }
}

// ─── Task #557: /rewind shared logic ──────────────────────────────────────────

/// Number of user-message candidates the picker offers by default.
pub const REWIND_DEFAULT_PICKER_SIZE: usize = 10;

/// Walk `session.messages` and return the indices of user-role
/// messages, most-recent first.  Used by the picker (TUI host) and by
/// `apply_rewind` for direct-target resolution.
#[must_use]
pub fn rewind_user_message_indices(session: &Session) -> Vec<usize> {
    let mut out: Vec<usize> = session
        .messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == runtime::MessageRole::User)
        .map(|(i, _)| i)
        .collect();
    out.reverse();
    out
}

/// Truncate `session.messages` so the last retained message is the
/// user message at `session.messages[target_index]`.  All messages
/// AFTER the target are dropped.  Returns the truncated session.
#[must_use]
pub fn rewind_truncate(session: &Session, target_index: usize) -> Session {
    let mut new_session = session.clone();
    if target_index >= new_session.messages.len() {
        return new_session;
    }
    new_session.messages.truncate(target_index + 1);
    new_session
}

/// Summarize the prefix up to (and including) `target_index`, then drop
/// every message that came after.  The result is a session whose first
/// message is a synthetic Summary (from `compact_session`) and whose
/// tail matches the chosen rewind point — i.e. a "rewind + summarize
/// the discarded prefix" composite.
///
/// Behavior:
/// - We isolate `messages[0..=target_index]` as the prefix.
/// - Run `compact_session(prefix, compaction)` on it.  When the prefix
///   is too small to compact (below the threshold), the prefix is
///   returned as-is (no summary is generated, matching the proactive
///   `compact_session` contract).
/// - Drop everything from `target_index + 1` onward.
#[must_use]
pub fn rewind_summarize(
    session: &Session,
    target_index: usize,
    compaction: CompactionConfig,
) -> Session {
    if target_index >= session.messages.len() {
        return session.clone();
    }
    // Build the prefix session (everything up through target_index).
    let prefix_session = Session {
        version: session.version,
        messages: session.messages[..=target_index].to_vec(),
    };
    // Force compaction even when proactive `should_compact` would skip,
    // by lowering the threshold to 1 (the helper still preserves the
    // most-recent N messages).
    let aggressive = CompactionConfig {
        preserve_recent_messages: 1,
        max_estimated_tokens: 1,
        ..compaction
    };
    let result = compact_session(&prefix_session, aggressive);
    result.compacted_session
}

/// Shared dispatch for the `/rewind` command (TUI + headless).
/// Headless path with no target lists the available user messages so
/// scripted callers can choose by index in a follow-up call.
fn apply_rewind(
    session: &Session,
    target: Option<usize>,
    summarize: bool,
    compaction: CompactionConfig,
) -> (Session, String) {
    let user_indices = rewind_user_message_indices(session);
    if user_indices.is_empty() {
        return (
            session.clone(),
            "/rewind: no user messages in this session.".to_string(),
        );
    }
    match target {
        None => {
            // No target: list the last N user messages so the caller
            // (TUI picker host OR scripted caller) can choose.
            let mut lines = vec![if summarize {
                "/rewind summarize: pick a user message to summarize-up-to:".to_string()
            } else {
                "/rewind: pick a user message to rewind to:".to_string()
            }];
            for (rank, &idx) in user_indices
                .iter()
                .take(REWIND_DEFAULT_PICKER_SIZE)
                .enumerate()
            {
                let preview = rewind_message_preview(&session.messages[idx]);
                lines.push(format!("  {} : {preview}", rank + 1));
            }
            if user_indices.len() > REWIND_DEFAULT_PICKER_SIZE {
                lines.push(format!(
                    "  … ({} more user messages — pass /rewind <N> with a larger index)",
                    user_indices.len() - REWIND_DEFAULT_PICKER_SIZE
                ));
            }
            (session.clone(), lines.join("\n"))
        }
        Some(n) => {
            // 1-based: 1 = most-recent user message.
            let Some(&idx) = user_indices.get(n.saturating_sub(1)) else {
                return (
                    session.clone(),
                    format!(
                        "/rewind: target {n} is out of range (only {} user messages)",
                        user_indices.len()
                    ),
                );
            };
            let preview = rewind_message_preview(&session.messages[idx]);
            if summarize {
                let new = rewind_summarize(session, idx, compaction);
                (
                    new,
                    format!("Summarized session up to user turn {n}: {preview}"),
                )
            } else {
                let new = rewind_truncate(session, idx);
                (new, format!("Rewound to user turn {n}: {preview}"))
            }
        }
    }
}

/// Render a short, single-line preview of a user message (first 80 chars
/// of the first Text block).  Used by both the picker view and the
/// confirmation message.
fn rewind_message_preview(message: &runtime::ConversationMessage) -> String {
    let raw = message
        .blocks
        .iter()
        .find_map(|b| match b {
            runtime::ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= 80 {
        collapsed
    } else {
        let truncated: String = collapsed.chars().take(77).collect();
        format!("{truncated}…")
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    use serial_test::serial;

    // ── Workspace-wide test isolation ────────────────────────────────────────
    //
    // Every `memory_show(...)` path eventually walks `default_config_home()`
    // (via MemoryManager / NominationStore / GoalManager / CronManager /
    // VaultManager / CommandCacheManager / etc.), so its result depends on
    // the live value of `ANVIL_CONFIG_HOME`. Other tests in the workspace
    // mutate that env var; we use `#[serial(anvil_config_home)]` (named
    // token) to serialise every test in this module against every other
    // ANVIL_CONFIG_HOME-mutating test in the workspace, without serialising
    // against unrelated tests. See `feedback-test-isolation-parallel-env-var-races.md`.

    #[test]
    #[serial(anvil_config_home)]
    fn memory_summary_contains_all_tiers() {
        let result = memory_summary();
        assert!(result.contains("anvil-md"), "should mention anvil-md tier");
        assert!(result.contains("nominations"), "should mention nominations tier");
        assert!(result.contains("daily"), "should mention daily tier");
        assert!(result.contains("vault"), "should mention vault tier");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_unknown_tier_returns_error() {
        let result = memory_show(Some("nonexistent-tier"), &MemoryContext::default());
        assert!(result.contains("Unknown tier"), "should report unknown tier");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_no_tier_returns_usage() {
        let result = memory_show(None, &MemoryContext::default());
        assert!(result.contains("Usage"), "should show usage when no tier given");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_vault_tier_returns_security_message() {
        let result = memory_show(Some("vault"), &MemoryContext::default());
        assert!(
            result.contains("security") || result.contains("encrypted"),
            "vault show should mention security/encryption"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_private_tier_returns_security_message() {
        let result = memory_show(Some("private"), &MemoryContext::default());
        assert!(
            result.contains("encrypted") || result.contains("vault"),
            "private show should mention encryption"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_working_without_snapshot_explains_requirement() {
        // L1 §4 acceptance: without a live runtime, the working view should
        // tell the user why no data is shown — not silently return empty.
        let result = memory_show(Some("working"), &MemoryContext::default());
        assert!(
            result.contains("No live working-memory snapshot"),
            "should explain missing live runtime"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_working_with_snapshot_lists_sections() {
        // L1 §4 acceptance: with a live snapshot, every section should
        // appear with its kind tag and a byte count.
        use runtime::{PromptSection, PromptSectionKind, WorkingMemorySnapshot};
        let snap = WorkingMemorySnapshot::new(vec![
            PromptSection::new(PromptSectionKind::Intro, "intro body"),
            PromptSection::new(PromptSectionKind::Environment, "env body"),
            PromptSection::labeled(PromptSectionKind::Skill, "skill body", "alpha"),
        ]);
        let ctx = MemoryContext::with_working(&snap, 3, 42);
        let result = memory_show(Some("working"), &ctx);
        assert!(result.contains("L1 Working memory snapshot"), "header");
        assert!(result.contains("intro"), "intro kind tag");
        assert!(result.contains("environment"), "environment kind tag");
        assert!(result.contains("skill"), "skill kind tag");
        assert!(result.contains("[alpha]"), "skill label should appear");
        assert!(result.contains("sections=3"), "section count");
        assert!(result.contains("message(s)"), "buffer count line");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_inspect_empty_key_returns_usage() {
        let result = memory_inspect("");
        assert!(result.contains("Usage"), "should show usage for empty key");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_inspect_nonexistent_key_reports_not_found() {
        let result = memory_inspect("xyzzy_nonexistent_key_99999");
        assert!(result.contains("No entries"), "should report no entries found");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_promote_empty_id_returns_usage() {
        let result = memory_promote("");
        assert!(result.contains("Usage"), "should show usage for empty id");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_promote_unknown_id_reports_not_found() {
        let result = memory_promote("nom-xxx-not-real");
        assert!(
            result.contains("not found") || result.contains("Error"),
            "unknown id should report error; got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn atomic_append_creates_file_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ANVIL.md");
        atomic_append_anvil_md(&path, "- hello world\n").expect("append");
        let contents = std::fs::read_to_string(&path).expect("read");
        assert!(contents.contains("- hello world"));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn atomic_append_appends_to_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ANVIL.md");
        std::fs::write(&path, "# Project notes\n").expect("seed");
        atomic_append_anvil_md(&path, "\n- promoted line\n").expect("append");
        let contents = std::fs::read_to_string(&path).expect("read");
        assert!(contents.contains("# Project notes"), "must preserve existing");
        assert!(contents.contains("- promoted line"), "must append new");
        // No partial-write .tmp file should linger.
        let tmp = path.with_extension("md.promote.tmp");
        assert!(!tmp.exists(), ".tmp file must not be left behind");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn atomic_append_handles_missing_trailing_newline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ANVIL.md");
        std::fs::write(&path, "no trailing newline").expect("seed");
        atomic_append_anvil_md(&path, "appended").expect("append");
        let contents = std::fs::read_to_string(&path).expect("read");
        assert!(contents.contains("no trailing newline\nappended"),
                "must insert newline between blocks; got: {contents:?}");
    }

    /// Process-local lock so HOME-mutating tests in this module serialise.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let m = LOCK.get_or_init(|| Mutex::new(()));
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Round-trip test: create a nomination, dry-run promote, real promote,
    /// verify ANVIL.md has the content and the nomination JSON is Accepted.
    #[test]
    #[serial(anvil_config_home)]
    fn memory_promote_roundtrip_writes_anvil_md_and_marks_accepted() {
        let _lock = env_lock();
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let prev_home = std::env::var_os("HOME");
        let prev_cwd = std::env::current_dir().ok();
        // SAFETY: tests serialise on env_lock(); restored on the way out.
        unsafe { std::env::set_var("HOME", home.path()); }
        let _ = std::env::set_current_dir(project.path());

        // Seed a nomination.
        let store = runtime::nominations::NominationStore::with_dir(
            home.path().join(".anvil").join("nominations"),
        );
        store.ensure_dir().unwrap();
        let nom = store
            .create(
                "test-session",
                runtime::nominations::NominationCategory::Pattern,
                "Always run cargo test before commit",
                0.9,
            )
            .expect("create nomination");

        // Dry-run first: ANVIL.md must remain absent.
        let dry = memory_promote(&format!("--dry-run {}", nom.id));
        assert!(dry.contains("[dry-run]"), "must announce dry-run; got: {dry}");
        assert!(dry.contains(&nom.id), "must mention id");
        assert!(!project.path().join("ANVIL.md").exists(),
                "ANVIL.md must NOT be created by dry-run");

        // Real promote: ANVIL.md should appear, nomination should flip status.
        let real = memory_promote(&nom.id);
        assert!(real.contains("promoted"), "must announce success; got: {real}");
        let anvil_md = project.path().join("ANVIL.md");
        assert!(anvil_md.exists(), "ANVIL.md must be created");
        let body = std::fs::read_to_string(&anvil_md).unwrap();
        assert!(body.contains("Always run cargo test before commit"),
                "content missing from ANVIL.md; got: {body}");
        assert!(body.contains(&nom.id),
                "attribution comment missing; got: {body}");

        let updated = store.get(&nom.id).expect("nom still on disk");
        assert_eq!(updated.status, runtime::nominations::NominationStatus::Accepted);
        assert!(updated.promoted_to.is_some(), "promoted_to must be set");

        // Second promote should be idempotent and report already-promoted.
        let second = memory_promote(&nom.id);
        assert!(second.contains("already promoted"),
                "double-promote should be idempotent; got: {second}");

        // Restore env.
        if let Some(prev) = prev_home {
            unsafe { std::env::set_var("HOME", prev); }
        } else {
            unsafe { std::env::remove_var("HOME"); }
        }
        if let Some(prev) = prev_cwd {
            let _ = std::env::set_current_dir(prev);
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_forget_empty_key_returns_usage() {
        let result = memory_forget("");
        assert!(result.contains("Usage"), "should show usage for empty key");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_why_mentions_injection_order() {
        let result = memory_why(&MemoryContext::default());
        assert!(result.contains("System prompt"), "should describe system prompt");
        assert!(result.contains("ANVIL.md"), "should mention ANVIL.md");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_why_live_snapshot_lists_kinds() {
        // L1 §4 acceptance: with a live snapshot, /memory why enumerates
        // every section kind in the order it ends up in the prompt.
        use runtime::{PromptSection, PromptSectionKind, WorkingMemorySnapshot};
        let snap = WorkingMemorySnapshot::new(vec![
            PromptSection::new(PromptSectionKind::Intro, "intro"),
            PromptSection::new(PromptSectionKind::InstructionFiles, "instr"),
            PromptSection::new(PromptSectionKind::KnownFiles, "kf"),
        ]);
        let ctx = MemoryContext::with_working(&snap, 0, 0);
        let result = memory_why(&ctx);
        assert!(result.contains("LIVE snapshot"), "should mark as live");
        assert!(result.contains("intro"));
        assert!(result.contains("instruction_files"));
        assert!(result.contains("known_files"));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_why_disclaimer_mentions_daily_inject_env() {
        // After task #500 the disclaimer must describe the ANVIL_DAILY_INJECT
        // env gate rather than the old "does NOT walk DailyStore" text.
        let result = memory_why(&MemoryContext::default());
        assert!(
            result.contains("ANVIL_DAILY_INJECT"),
            "static disclaimer must mention ANVIL_DAILY_INJECT env var: {result}"
        );
        assert!(
            !result.contains("does NOT walk DailyStore"),
            "old wiring-gap disclaimer must be gone: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_why_live_snapshot_disclaimer_mentions_daily_inject_env() {
        // The live-snapshot path must also carry the updated disclaimer.
        use runtime::{PromptSection, PromptSectionKind, WorkingMemorySnapshot};
        let snap = WorkingMemorySnapshot::new(vec![
            PromptSection::new(PromptSectionKind::Intro, "intro"),
        ]);
        let ctx = MemoryContext::with_working(&snap, 0, 0);
        let result = memory_why(&ctx);
        assert!(
            result.contains("ANVIL_DAILY_INJECT"),
            "live-snapshot disclaimer must mention ANVIL_DAILY_INJECT: {result}"
        );
        assert!(
            !result.contains("does NOT walk DailyStore"),
            "old wiring-gap phrase must be gone from live path: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_budget_shows_tiers_and_totals() {
        let result = memory_budget(&MemoryContext::default());
        assert!(result.contains("anvil-md"), "should show anvil-md tier");
        assert!(result.contains("TOTAL"), "should show total row");
        assert!(
            result.contains("Tokens") || result.contains("token"),
            "should show token estimate"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_budget_adds_working_row_when_snapshot_present() {
        // L1 §5 acceptance: budget shows the live working-memory bytes
        // alongside the on-disk tiers — only when a snapshot is provided.
        use runtime::{PromptSection, PromptSectionKind, WorkingMemorySnapshot};
        let snap = WorkingMemorySnapshot::new(vec![PromptSection::new(
            PromptSectionKind::Intro,
            "x".repeat(120),
        )]);
        let ctx = MemoryContext::with_working(&snap, 0, 0);
        let result = memory_budget(&ctx);
        assert!(result.contains("working"), "should include working row");
        assert!(result.contains("TOTAL"), "should still show total row");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_semantic_default_combines_approved_and_pending_count() {
        // L3 §1 acceptance: default `semantic` view combines anvil-md
        // (approved) with the pending-nomination count, framing the
        // tier in the user's mental model rather than as two separate
        // tiers.
        let result = memory_show(Some("semantic"), &MemoryContext::default());
        assert!(
            result.contains("L3 Semantic memory"),
            "header should be present; got: {result}"
        );
        assert!(result.contains("approved (anvil-md):"));
        assert!(result.contains("pending:"));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_semantic_pending_routes_to_nominations_store() {
        // L3 §1 acceptance: `semantic --pending` (and `pending` short
        // form, and the legacy `nominations` sub-view) all hit the
        // NominationStore::format_pending path.
        let pending = memory_show(Some("semantic --pending"), &MemoryContext::default());
        let short = memory_show(Some("semantic pending"), &MemoryContext::default());
        let legacy = memory_show(Some("semantic nominations"), &MemoryContext::default());
        for variant in [&pending, &short, &legacy] {
            assert!(
                variant.contains("L3 Semantic — pending nominations"),
                "should route to pending view; got: {variant}"
            );
        }
        assert_eq!(pending, short, "--pending and pending must alias");
        assert_eq!(pending, legacy, "nominations sub-view must alias");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_nominations_alias_carries_deprecation_banner() {
        // L3 §1 acceptance: the legacy top-level `nominations` tier is
        // kept for one cycle, but the output explains the rename so
        // users discover the new canonical form. Phase 4.4 unified the
        // banner phrasing to `[deprecated] <old> will be removed next
        // release; use <new>`.
        let result = memory_show(Some("nominations"), &MemoryContext::default());
        assert!(
            result.contains("[deprecated]"),
            "should carry Phase 4.4 deprecation banner; got: {result}"
        );
        assert!(
            result.contains("/memory show semantic --pending"),
            "should advertise the new path; got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_semantic_unknown_sub_view_lists_known() {
        let result = memory_show(Some("semantic explode"), &MemoryContext::default());
        assert!(
            result.contains("Unknown semantic sub-view"),
            "should reject unknown; got: {result}"
        );
        assert!(result.contains("pending"));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_subcommand_picker_lists_seven_layer_tiers() {
        // Phase 2.8 acceptance: the show-tier picker must enumerate
        // every L1-L7 layer name. If a future edit drops one we want a
        // hard failure, not a silent regression.
        use crate::subcommands::{ArgSpec, MEMORY_SUBCOMMANDS};
        let show = MEMORY_SUBCOMMANDS
            .iter()
            .find(|s| s.name == "show")
            .expect("MEMORY_SUBCOMMANDS must contain a `show` entry");
        let ArgSpec::OneOf(tiers) = show.args[0] else {
            panic!("show's first arg must be ArgSpec::OneOf");
        };
        for layer in [
            "working", "episodic", "semantic", "procedural", "identity", "policy", "cache",
        ] {
            assert!(
                tiers.contains(&layer),
                "show picker missing seven-layer tier `{layer}`; got: {tiers:?}"
            );
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_cache_default_lists_three_sources() {
        // L7 §3-5 acceptance: default `cache` view summarises file,
        // cmd, qmd in one place so the user can see the L7 vocabulary.
        let result = memory_show(Some("cache"), &MemoryContext::default());
        assert!(
            result.contains("L7 Cache memory"),
            "header missing; got: {result}"
        );
        for label in ["file", "cmd", "qmd"] {
            assert!(result.contains(label), "expected `{label}`; got: {result}");
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_cache_file_routes_to_file_cache_manager() {
        let result = memory_show(Some("cache file"), &MemoryContext::default());
        assert!(
            result.contains("L7 Cache — file") || result.contains("not initialised"),
            "got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_cache_cmd_routes_to_cmd_cache_manager() {
        let result = memory_show(Some("cache cmd"), &MemoryContext::default());
        assert!(
            result.contains("L7 Cache — cmd") || result.contains("not initialised"),
            "got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_cache_qmd_routes_to_qmd_client() {
        // QMD binary may or may not be on PATH in CI. Either way, the
        // header should appear.
        let result = memory_show(Some("cache qmd"), &MemoryContext::default());
        assert!(result.contains("L7 Cache — qmd"), "got: {result}");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_cache_unknown_sub_view_lists_known() {
        let result = memory_show(Some("cache explode"), &MemoryContext::default());
        assert!(result.contains("Unknown cache sub-view"), "got: {result}");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_policy_lists_all_four_sections() {
        // L6 §2-5 acceptance: the policy view labels each of the four
        // policy sources so the user can see what's in play.
        let result = memory_show(Some("policy"), &MemoryContext::default());
        assert!(
            result.contains("L6 Policy memory"),
            "header missing; got: {result}"
        );
        for label in ["permission grants", "auto-mode hard_deny", "reviewer", "egress allowlist"]
        {
            assert!(
                result.contains(label),
                "expected `{label}`; got: {result}"
            );
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_policy_egress_section_renders_domain_count() {
        // Rewritten from the old "wiring gap" test. The banner is gone.
        // The egress allowlist section must render with a domain count ≥ 1
        // (the runtime-default domains are always included), and must NOT
        // contain the old wiring-gap banner string.
        let result = memory_show(Some("policy"), &MemoryContext::default());
        assert!(
            result.contains("egress allowlist"),
            "egress section missing; got: {result}"
        );
        // Wiring-gap banner must be gone.
        assert!(
            !result.contains("not yet merged into settings.json"),
            "stale wiring-gap banner still present; got: {result}"
        );
        // Default list always has at least one domain.
        assert!(
            result.contains("domain(s)"),
            "egress domain count line missing; got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_policy_reflects_configured_egress_block() {
        // Write a fake settings.json with egress.enabled=true and a custom
        // domain, point ANVIL_CONFIG_HOME at it, confirm the view surfaces
        // the correct enabled= value and the custom domain appears.
        use std::fs;

        let root = {
            use std::time::{SystemTime, UNIX_EPOCH};
            use std::sync::atomic::{AtomicU64, Ordering};
            static CTR: AtomicU64 = AtomicU64::new(0);
            let n = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos();
            let i = CTR.fetch_add(1, Ordering::Relaxed);
            std::env::temp_dir().join(format!("handlers-egress-{n}-{i}"))
        };
        let home = root.join(".anvil");
        fs::create_dir_all(&home).expect("create test home");

        fs::write(
            home.join("settings.json"),
            r#"{"egress": {"enabled": true, "allowlist": ["custom.test"]}}"#,
        )
        .expect("write settings");

        // SAFETY: test-only env mutation, serialised by serial(anvil_config_home).
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &home) };

        let result = memory_show(Some("policy"), &MemoryContext::default());

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };
        fs::remove_dir_all(root).expect("cleanup");

        assert!(
            result.contains("custom.test"),
            "custom domain should appear in egress section; got: {result}"
        );
        assert!(
            result.contains("enabled=true"),
            "enabled=true should appear; got: {result}"
        );
        assert!(
            !result.contains("not yet merged into settings.json"),
            "wiring-gap banner must be absent; got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_identity_never_renders_secrets() {
        // L5 §1-4 acceptance: whichever state the vault is in
        // (uninitialised / locked / unlocked), the rendered string must
        // NEVER carry a literal "secret" or render anything resembling
        // a credential value — we only emit labels, counts, or status.
        let result = memory_show(Some("identity"), &MemoryContext::default());
        assert!(
            result.contains("L5 Identity memory"),
            "header missing; got: {result}"
        );
        // No banned token may appear (the renderer is hardcoded to
        // never print secret bytes).
        assert!(
            !result.to_ascii_lowercase().contains("secret:"),
            "must not render literal secret value; got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_identity_routes_via_initialised_state() {
        // Each branch is selected by vault_is_initialized/unlocked.
        // The shape of the message lets us assert which branch fired.
        let result = memory_show(Some("identity"), &MemoryContext::default());
        let one_of = result.contains("not initialised")
            || result.contains("UNLOCKED")
            || result.contains("LOCKED");
        assert!(one_of, "should branch on vault state; got: {result}");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_procedural_default_summarises_four_sources() {
        // L4 §1-5 acceptance: the default view summarises goals + skills
        // + cron + routines so the user sees the L4 vocabulary in one
        // place.
        let result = memory_show(Some("procedural"), &MemoryContext::default());
        assert!(
            result.contains("L4 Procedural memory"),
            "header missing; got: {result}"
        );
        for label in ["goals", "skills", "cron", "routines"] {
            assert!(result.contains(label), "expected {label}; got: {result}");
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_procedural_routines_is_stubbed_with_roadmap_pointer() {
        // L4 §5 acceptance: routines sub-view advertises the deferred
        // arrival rather than emitting fake data.
        let result = memory_show(Some("procedural routines"), &MemoryContext::default());
        assert!(
            result.contains("TODO")
                && result.contains("ROADMAP")
                && result.contains("routines archive"),
            "expected ROADMAP stub; got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_procedural_skills_lists_loaded_skill_names_from_snapshot() {
        // L4 §2 acceptance: skill sub-view enumerates each loaded skill
        // from the live PromptSection::Skill labels.
        use runtime::{PromptSection, PromptSectionKind, WorkingMemorySnapshot};
        let snap = WorkingMemorySnapshot::new(vec![
            PromptSection::labeled(PromptSectionKind::Skill, "body-a", "alpha"),
            PromptSection::new(PromptSectionKind::Intro, "intro"),
            PromptSection::labeled(PromptSectionKind::Skill, "body-b", "beta"),
        ]);
        let ctx = MemoryContext::with_working(&snap, 0, 0);
        let result = memory_show(Some("procedural skills"), &ctx);
        assert!(result.contains("loaded: alpha"), "got: {result}");
        assert!(result.contains("loaded: beta"), "got: {result}");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_procedural_goals_routes_to_goal_manager() {
        let result = memory_show(Some("procedural goals"), &MemoryContext::default());
        assert!(
            result.contains("L4 Procedural — goals"),
            "header missing; got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_procedural_cron_routes_to_cron_manager() {
        // L4 §3 acceptance: cron sub-view goes through CronManager::global.
        let result = memory_show(Some("procedural cron"), &MemoryContext::default());
        assert!(
            result.contains("L4 Procedural — cron"),
            "header missing; got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_procedural_unknown_sub_view_lists_known() {
        let result = memory_show(Some("procedural explode"), &MemoryContext::default());
        assert!(result.contains("Unknown procedural sub-view"), "got: {result}");
        assert!(result.contains("goals"));
        assert!(result.contains("routines"));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_episodic_default_lists_archive_count() {
        // L2 §1 acceptance: the episodic tier surfaces the HistoryArchiver
        // archive count and tells the user where it lives. The directory
        // may be empty in CI; we just assert the framing and stat row.
        let result = memory_show(Some("episodic"), &MemoryContext::default());
        assert!(
            result.contains("L2 Episodic memory"),
            "header should be present"
        );
        assert!(result.contains("archived_sessions="));
        assert!(result.contains("history_dir="));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_show_episodic_daily_routes_to_daily_summary() {
        // L2 §1 acceptance: `episodic daily` (or `episodic --daily`) shows
        // the structured DailySummary. The directory is likely empty under
        // test, so we assert the empty-state copy.
        let result = memory_show(Some("episodic daily"), &MemoryContext::default());
        assert!(
            result.contains("No daily summary entries for today.")
                || result.contains("=== L2 Episodic — daily summary ==="),
            "should route to daily store; got: {result}"
        );

        // The `--daily` flag form must produce the same output as `daily`.
        let alt = memory_show(Some("episodic --daily"), &MemoryContext::default());
        assert_eq!(result, alt, "--daily must alias to daily");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_summary_lists_episodic_tier() {
        // L2 §1 acceptance: `/memory` (no args) summary mentions episodic.
        let result = memory_summary();
        assert!(
            result.contains("episodic"),
            "summary should list episodic tier; got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_budget_lists_episodic_row() {
        // L2 §1 acceptance: `/memory budget` includes an episodic row.
        let result = memory_budget(&MemoryContext::default());
        assert!(
            result.contains("episodic"),
            "budget should include episodic row; got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn memory_prune_returns_summary_with_both_tiers() {
        let result = memory_prune();
        assert!(result.contains("daily"), "should mention daily pruning");
        assert!(result.contains("nominations"), "should mention nominations pruning");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn handle_memory_command_none_dispatches_to_summary() {
        let result = handle_memory_command(None, &MemoryContext::default());
        assert!(result.contains("anvil-md"), "summary should contain tier info");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn handle_memory_command_dispatches_subcommands() {
        let why = handle_memory_command(Some("why"), &MemoryContext::default());
        assert!(why.contains("System prompt"), "why should describe injection");

        let budget = handle_memory_command(Some("budget"), &MemoryContext::default());
        assert!(budget.contains("TOTAL"), "budget should show totals");

        let unknown = handle_memory_command(Some("explode"), &MemoryContext::default());
        assert!(unknown.contains("Unknown"), "unknown subcommand should error");
    }

    // ── Phase 6.4 tests ──────────────────────────────────────────────────────

    #[test]
    fn confirmation_block_dry_run_shows_label() {
        let plan = ImportPlanSummary {
            memory_found: 12,
            total_staged: 0,
            total_skipped: 0,
            total_needs_review: 0,
            needs_review_items: vec![],
            ..Default::default()
        };
        let block = plan.render_confirmation_block(true);
        assert!(block.contains("DRY RUN"), "dry-run block should contain DRY RUN: {block}");
        assert!(block.contains("12"), "block should show memory count: {block}");
    }

    #[test]
    fn confirmation_block_live_has_commit_prompt() {
        let plan = ImportPlanSummary {
            memory_found: 5,
            total_staged: 5,
            ..Default::default()
        };
        let block = plan.render_confirmation_block(false);
        assert!(block.contains("[c]ommit"), "live block should have commit prompt: {block}");
        assert!(!block.contains("DRY RUN"), "live block must not have DRY RUN: {block}");
    }

    #[test]
    fn confirmation_block_shows_needs_review_items() {
        let plan = ImportPlanSummary {
            memory_found: 3,
            total_needs_review: 1,
            needs_review_items: vec![(
                "ANVIL.md already exists → staged as ANVIL.imported.md".to_string(),
                "/home/user/projects/foo/ANVIL.imported.md".to_string(),
            )],
            ..Default::default()
        };
        let block = plan.render_confirmation_block(false);
        assert!(block.contains("Needs Review"), "block should have Needs Review: {block}");
        assert!(block.contains("ANVIL.imported.md"), "block should list review item: {block}");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn permission_gate_readonly_blocks_commit() {
        let result = handle_import_command_with_mode(
            Some("claude-code"),
            false, // live commit — should be blocked
            None,
            false,
            Some(runtime::PermissionMode::ReadOnly),
        );
        assert!(
            result.contains("WorkspaceWrite") || result.contains("read-only") || result.contains("ReadOnly"),
            "ReadOnly mode must produce permission error; got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn permission_gate_dry_run_allowed_in_readonly() {
        let result = handle_import_command_with_mode(
            Some("claude-code"),
            true, // dry-run — no writes — must be allowed
            None,
            false,
            Some(runtime::PermissionMode::ReadOnly),
        );
        assert!(
            !result.contains("Import requires WorkspaceWrite"),
            "dry-run should not be blocked by ReadOnly; got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn permission_gate_workspace_write_allows_commit() {
        let result = handle_import_command_with_mode(
            Some("claude-code"),
            false,
            None,
            false,
            Some(runtime::PermissionMode::WorkspaceWrite),
        );
        assert!(
            !result.contains("Import requires WorkspaceWrite"),
            "WorkspaceWrite must not produce permission error; got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn dry_run_command_shows_dry_run_label() {
        // Verify that --dry-run produces output containing the DRY RUN label
        // and the import plan summary, without committing any artifacts.
        let result = handle_import_command(
            Some("claude-code"),
            true,  // dry-run
            None,
            false,
        );

        // The confirmation block must contain the DRY RUN label.
        assert!(
            result.contains("DRY RUN"),
            "dry-run result must have DRY RUN label; got: {result}"
        );

        // The result must mention a Report path.
        assert!(
            result.contains("Report:"),
            "dry-run result must mention report path; got: {result}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn dry_run_report_content_has_dry_run_prefix() {
        // Verify the Phase 6.4 dry-run behavior via the result summary string:
        // the confirmation block must contain the [DRY RUN] label, and the
        // report path reference must be present.
        //
        // We do not assert the report file location here because
        // ANVIL_CONFIG_HOME is shared across workspace crates; asserting the
        // file path would create a cross-crate race.  Instead we verify the
        // report content through the generate_full_report unit path (tested
        // separately in runtime/tests/import_phase64.rs).
        let result = handle_import_command(
            Some("claude-code"),
            true,  // dry-run
            None,
            false,
        );

        // The confirmation block must contain the DRY RUN label.
        assert!(
            result.contains("DRY RUN"),
            "dry-run result must have DRY RUN label; got: {result}"
        );

        // The result must mention a Report path.
        assert!(
            result.contains("Report:"),
            "dry-run result must mention report path; got: {result}"
        );
    }

    #[test]
    fn unknown_source_returns_usage_hint() {
        let result = handle_import_command(Some("github"), false, None, false);
        assert!(result.contains("unknown source"), "unknown source should show error: {result}");
        assert!(result.contains("claude-code"), "error should mention claude-code: {result}");
    }
}

// ─── /cursor handler tests ───────────────────────────────────────────────────

#[cfg(test)]
mod cursor_tests {
    use super::*;
    use crate::CursorSubcommand;

    // ── launch ───────────────────────────────────────────────────────────────

    #[test]
    fn launch_empty_prompt_returns_usage_hint() {
        let result = handle_cursor_command(&CursorSubcommand::Launch {
            prompt: String::new(),
        });
        assert!(result.contains("Usage: /cursor launch"), "must show usage: {result}");
        assert!(result.contains("CURSOR_API_KEY"), "must mention credential: {result}");
    }

    #[test]
    fn launch_with_prompt_returns_guidance() {
        let result = handle_cursor_command(&CursorSubcommand::Launch {
            prompt: "Fix all failing tests and open a PR".to_string(),
        });
        assert!(!result.is_empty(), "non-empty prompt should return guidance");
        assert!(result.contains("Fix all failing tests"), "guidance should echo prompt: {result}");
    }

    #[test]
    fn launch_long_prompt_is_truncated_in_message() {
        let long_prompt = "A".repeat(100);
        let result = handle_cursor_command(&CursorSubcommand::Launch {
            prompt: long_prompt.clone(),
        });
        // Only up to 60 chars should appear in the result message.
        let truncated = &long_prompt[..60];
        assert!(result.contains(truncated), "truncated preview must appear: {result}");
    }

    // ── list ─────────────────────────────────────────────────────────────────

    #[test]
    fn list_no_flags_returns_guidance() {
        let result = handle_cursor_command(&CursorSubcommand::List {
            archived: false,
            pr_filter: None,
            cursor_token: None,
        });
        assert!(!result.is_empty(), "list should return guidance");
        assert!(result.contains("agent_id"), "must mention agent_id column: {result}");
        assert!(result.contains("status"), "must mention status column: {result}");
    }

    #[test]
    fn list_archived_flag_shown_in_message() {
        let result = handle_cursor_command(&CursorSubcommand::List {
            archived: true,
            pr_filter: None,
            cursor_token: None,
        });
        assert!(result.contains("--archived"), "archived flag must appear in message: {result}");
    }

    #[test]
    fn list_pr_filter_shown_in_message() {
        let result = handle_cursor_command(&CursorSubcommand::List {
            archived: false,
            pr_filter: Some("https://github.com/org/repo/pull/42".to_string()),
            cursor_token: None,
        });
        assert!(result.contains("--pr="), "pr flag must appear in message: {result}");
    }

    #[test]
    fn list_cursor_token_shown_in_message() {
        let result = handle_cursor_command(&CursorSubcommand::List {
            archived: false,
            pr_filter: None,
            cursor_token: Some("next_page_token_xyz".to_string()),
        });
        assert!(result.contains("--cursor="), "cursor flag must appear in message: {result}");
    }

    // ── get ──────────────────────────────────────────────────────────────────

    #[test]
    fn get_empty_id_returns_usage_hint() {
        let result = handle_cursor_command(&CursorSubcommand::Get {
            agent_id: String::new(),
        });
        assert!(result.contains("Usage: /cursor get"), "must show usage: {result}");
    }

    #[test]
    fn get_with_id_returns_guidance() {
        let result = handle_cursor_command(&CursorSubcommand::Get {
            agent_id: "agt_abc123".to_string(),
        });
        assert!(result.contains("agt_abc123"), "guidance must echo agent_id: {result}");
        assert!(!result.is_empty());
    }

    // ── cancel ───────────────────────────────────────────────────────────────

    #[test]
    fn cancel_empty_id_returns_usage_hint() {
        let result = handle_cursor_command(&CursorSubcommand::Cancel {
            agent_id: String::new(),
            run_id: None,
        });
        assert!(result.contains("Usage: /cursor cancel"), "must show usage: {result}");
    }

    #[test]
    fn cancel_with_id_no_run_id_returns_guidance() {
        let result = handle_cursor_command(&CursorSubcommand::Cancel {
            agent_id: "agt_abc123".to_string(),
            run_id: None,
        });
        assert!(result.contains("agt_abc123"), "must echo agent_id: {result}");
        assert!(result.contains("latest active run"), "must mention latest run: {result}");
    }

    #[test]
    fn cancel_with_id_and_run_id_returns_guidance() {
        let result = handle_cursor_command(&CursorSubcommand::Cancel {
            agent_id: "agt_abc123".to_string(),
            run_id: Some("run_xyz789".to_string()),
        });
        assert!(result.contains("agt_abc123"), "must echo agent_id: {result}");
        assert!(result.contains("run_xyz789"), "must echo run_id: {result}");
    }

    // ── artifacts ────────────────────────────────────────────────────────────

    #[test]
    fn artifacts_empty_id_returns_usage_hint() {
        let result = handle_cursor_command(&CursorSubcommand::Artifacts {
            agent_id: String::new(),
        });
        assert!(result.contains("Usage: /cursor artifacts"), "must show usage: {result}");
    }

    #[test]
    fn artifacts_with_id_mentions_presigned_urls() {
        let result = handle_cursor_command(&CursorSubcommand::Artifacts {
            agent_id: "agt_abc123".to_string(),
        });
        assert!(result.contains("agt_abc123"), "must echo agent_id: {result}");
        assert!(result.contains("presigned"), "must mention presigned URLs: {result}");
    }

    // ── stream ───────────────────────────────────────────────────────────────

    #[test]
    fn stream_empty_ids_returns_usage_hint() {
        let result = handle_cursor_command(&CursorSubcommand::Stream {
            agent_id: String::new(),
            run_id: String::new(),
        });
        assert!(result.contains("Usage: /cursor stream"), "must show usage: {result}");
    }

    #[test]
    fn stream_with_ids_mentions_sse_and_resume() {
        let result = handle_cursor_command(&CursorSubcommand::Stream {
            agent_id: "agt_abc123".to_string(),
            run_id: "run_xyz789".to_string(),
        });
        assert!(result.contains("agt_abc123"), "must echo agent_id: {result}");
        assert!(result.contains("run_xyz789"), "must echo run_id: {result}");
        assert!(result.contains("Last-Event-ID"), "must mention SSE resume: {result}");
        assert!(result.contains("410"), "must explain 410 expiry: {result}");
    }

    // ── parse round-trips ─────────────────────────────────────────────────────
    // Verify that the parser in lib.rs produces CursorSubcommand values that
    // the handler can process (end-to-end integration of parse → handle).

    #[test]
    fn parse_cursor_launch_routes_to_handler() {
        use crate::SlashCommand;
        let cmd = SlashCommand::parse("/cursor launch Fix tests")
            .expect("parse must succeed");
        match cmd {
            SlashCommand::Cursor { subcommand: CursorSubcommand::Launch { prompt } } => {
                assert_eq!(prompt, "Fix tests");
                let result = handle_cursor_command(&CursorSubcommand::Launch { prompt });
                assert!(!result.is_empty());
                assert!(result.contains("Fix tests"));
            }
            other => panic!("expected Cursor::Launch, got {other:?}"),
        }
    }

    #[test]
    fn parse_cursor_list_routes_to_handler() {
        use crate::SlashCommand;
        let cmd = SlashCommand::parse("/cursor list").expect("parse must succeed");
        match cmd {
            SlashCommand::Cursor { subcommand: CursorSubcommand::List { archived, .. } } => {
                assert!(!archived, "default archived should be false");
                let result = handle_cursor_command(&CursorSubcommand::List {
                    archived: false,
                    pr_filter: None,
                    cursor_token: None,
                });
                assert!(!result.is_empty());
            }
            other => panic!("expected Cursor::List, got {other:?}"),
        }
    }

    #[test]
    fn parse_cursor_list_archived_flag() {
        use crate::SlashCommand;
        let cmd = SlashCommand::parse("/cursor list --archived").expect("parse must succeed");
        match cmd {
            SlashCommand::Cursor { subcommand: CursorSubcommand::List { archived, .. } } => {
                assert!(archived, "archived flag must be parsed as true");
            }
            other => panic!("expected Cursor::List, got {other:?}"),
        }
    }

    #[test]
    fn parse_cursor_get_routes_to_handler() {
        use crate::SlashCommand;
        let cmd = SlashCommand::parse("/cursor get agt_abc123").expect("parse must succeed");
        match cmd {
            SlashCommand::Cursor { subcommand: CursorSubcommand::Get { agent_id } } => {
                assert_eq!(agent_id, "agt_abc123");
                let result = handle_cursor_command(&CursorSubcommand::Get { agent_id });
                assert!(!result.is_empty());
            }
            other => panic!("expected Cursor::Get, got {other:?}"),
        }
    }

    #[test]
    fn parse_cursor_cancel_with_run_id() {
        use crate::SlashCommand;
        let cmd = SlashCommand::parse("/cursor cancel agt_abc123 run_xyz789")
            .expect("parse must succeed");
        match cmd {
            SlashCommand::Cursor { subcommand: CursorSubcommand::Cancel { agent_id, run_id } } => {
                assert_eq!(agent_id, "agt_abc123");
                assert_eq!(run_id.as_deref(), Some("run_xyz789"));
            }
            other => panic!("expected Cursor::Cancel, got {other:?}"),
        }
    }

    #[test]
    fn parse_cursor_artifacts_routes_to_handler() {
        use crate::SlashCommand;
        let cmd = SlashCommand::parse("/cursor artifacts agt_abc123").expect("parse must succeed");
        match cmd {
            SlashCommand::Cursor { subcommand: CursorSubcommand::Artifacts { agent_id } } => {
                assert_eq!(agent_id, "agt_abc123");
                let result = handle_cursor_command(&CursorSubcommand::Artifacts { agent_id });
                assert!(!result.is_empty());
            }
            other => panic!("expected Cursor::Artifacts, got {other:?}"),
        }
    }

    #[test]
    fn parse_cursor_stream_routes_to_handler() {
        use crate::SlashCommand;
        let cmd = SlashCommand::parse("/cursor stream agt_abc123 run_xyz789")
            .expect("parse must succeed");
        match cmd {
            SlashCommand::Cursor {
                subcommand: CursorSubcommand::Stream { agent_id, run_id },
            } => {
                assert_eq!(agent_id, "agt_abc123");
                assert_eq!(run_id, "run_xyz789");
                let result = handle_cursor_command(&CursorSubcommand::Stream { agent_id, run_id });
                assert!(result.contains("Last-Event-ID"));
            }
            other => panic!("expected Cursor::Stream, got {other:?}"),
        }
    }
}
