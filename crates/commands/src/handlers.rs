use runtime::{compact_session, CompactionConfig, Session, WorkingMemorySnapshot};

use crate::specs::{render_command_detailed_help, render_slash_command_help};
use crate::SlashCommand;

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
        SlashCommand::Help { ref command } => Some(SlashCommandResult {
            message: if let Some(cmd) = command {
                render_command_detailed_help(cmd)
                    .unwrap_or_else(render_slash_command_help)
            } else {
                render_slash_command_help()
            },
            session: session.clone(),
        }),
        SlashCommand::Status => Some(SlashCommandResult {
            message: "/status is not yet implemented. To check project status, ask the assistant directly or run `git status` and `git log --oneline` from your terminal.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Branch { .. } => Some(SlashCommandResult {
            message: "/branch is not yet implemented. Use `git branch` or `git checkout -b <name>` from your terminal to manage branches.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Bughunter { .. } => Some(SlashCommandResult {
            message: "/bughunter is not yet implemented. Describe the bug to the assistant and ask it to investigate — it can read files, run tests, and trace execution.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Worktree { .. } => Some(SlashCommandResult {
            message: "/worktree is not yet implemented. Use `git worktree add <path> <branch>` from your terminal to manage Git worktrees.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Commit => Some(SlashCommandResult {
            message: "/commit is not yet implemented. Stage your changes and run `git commit` from your terminal, or ask the assistant to help write a commit message.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::CommitPushPr { .. } => Some(SlashCommandResult {
            message: "/commit-push-pr is not yet implemented. To commit, push, and open a pull request, ask the assistant to help or run `git commit`, `git push`, and `gh pr create` from your terminal.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Pr { .. } => Some(SlashCommandResult {
            message: "/pr is not yet implemented. Use `gh pr create` or `gh pr list` from your terminal to manage pull requests.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Issue { .. } => Some(SlashCommandResult {
            message: "/issue is not yet implemented. Use `gh issue create` or `gh issue list` from your terminal to manage GitHub issues.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Ultraplan { .. } => Some(SlashCommandResult {
            message: "/ultraplan is not yet implemented. Ask the assistant to create a detailed implementation plan for your task instead.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Teleport { .. } => Some(SlashCommandResult {
            message: "/teleport is not yet implemented. This command would navigate to a specific file or symbol. Use your editor's go-to-definition or ask the assistant to locate the code for you.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::DebugToolCall => Some(SlashCommandResult {
            message: "/debug-tool-call is not yet implemented. To inspect tool call behavior, ask the assistant to explain what it is doing or enable verbose logging in your configuration.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Model { .. } => Some(SlashCommandResult {
            message: "/model is not yet implemented. Change the active model via the configuration file or the --model flag when starting Anvil.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Permissions { .. } => Some(SlashCommandResult {
            message: "/permissions is not yet implemented. Edit your settings.json to adjust tool permissions and allowed commands.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Clear { .. } => Some(SlashCommandResult {
            message: "/clear is not yet implemented. Start a new session to clear the conversation history.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Cost => Some(SlashCommandResult {
            message: "/cost is not yet implemented. Token usage and estimated cost are not yet tracked in this session.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Resume { .. } => Some(SlashCommandResult {
            message: "/resume is not yet implemented. Session resumption from a saved file is not yet supported.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Config { .. } => Some(SlashCommandResult {
            message: "/config is not yet implemented. Edit your settings.json file directly to change configuration values.".to_string(),
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
            message: "/init is not yet implemented. To initialize Anvil in a new project, create an ANVIL.md file in your project root with instructions for the assistant.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Diff => Some(SlashCommandResult {
            message: "/diff is not yet implemented. Run `git diff` or `git diff --staged` from your terminal to review pending changes.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Version => Some(SlashCommandResult {
            message: "/version is not yet implemented. Run `anvil --version` from your terminal to see the installed version.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Export { .. } => Some(SlashCommandResult {
            message: "/export is not yet implemented. To save the conversation, copy the output from your terminal or redirect stdout to a file when starting Anvil.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Session { .. } => Some(SlashCommandResult {
            message: "/session is not yet implemented. Session management (listing, switching, saving) is not yet available.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Plugins { .. } => Some(SlashCommandResult {
            message: "/plugins is not yet implemented here. Plugin management is handled at startup via the plugin configuration in settings.json.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Agents { .. } => Some(SlashCommandResult {
            message: "/agents is not yet implemented here. Agent definitions are loaded from your configured agent roots at session start.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Skills { .. } => Some(SlashCommandResult {
            message: "/skills is not yet implemented here. Skill definitions are loaded from your configured skill roots at session start.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Qmd { .. } => Some(SlashCommandResult {
            message: "/qmd is not yet implemented. The QMD knowledge base search is available as a tool — ask the assistant to search your documents instead.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Undo => Some(SlashCommandResult {
            message: "/undo is not yet implemented. To revert the last change, use `git checkout -- <file>` or ask the assistant to reverse the edit it just made.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::History { .. } => Some(SlashCommandResult {
            message: "/history is not yet implemented. Scroll up in your terminal to review the conversation, or run `git log --oneline` for commit history.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Context { .. } => Some(SlashCommandResult {
            message: "/context is not yet implemented. The assistant's active context is the current session — use /compact to reduce token usage if the session is getting long.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Pin { .. } => Some(SlashCommandResult {
            message: "/pin is not yet implemented. To keep important context available, add it to your ANVIL.md file so it is loaded at every session start.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Unpin { .. } => Some(SlashCommandResult {
            message: "/unpin is not yet implemented. Remove entries from your ANVIL.md file to stop including them in future sessions.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Chat => Some(SlashCommandResult {
            message: "/chat is not yet implemented. You are already in chat mode — type your message and press Enter to continue the conversation.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Vim => Some(SlashCommandResult {
            message: "/vim is not yet implemented. Vim keybinding mode is not yet supported. Use your terminal's readline bindings or configure your shell for vi mode.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Web { .. } => Some(SlashCommandResult {
            message: "/web is not yet implemented. Ask the assistant to fetch a URL using the WebFetch tool, or use `curl` from your terminal.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Doctor { .. } => Some(SlashCommandResult {
            message: "/doctor is not yet implemented. To diagnose configuration issues, check that your API key is set, your settings.json is valid JSON, and all required tools are installed.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Tokens => Some(SlashCommandResult {
            message: "/tokens is not yet implemented. Token counting for the current session is not yet exposed. Use /compact to reduce session size if needed.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Provider { .. } => Some(SlashCommandResult {
            message: "/provider is not yet implemented. Switch providers by updating the provider setting in your settings.json or by using the --provider flag at startup.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Login { .. } => Some(SlashCommandResult {
            message: "/login is not yet implemented. Set your API key via the ANTHROPIC_API_KEY environment variable or the anvil login command from your terminal.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Search { .. } => Some(SlashCommandResult {
            message: "/search is not yet implemented. Ask the assistant to search your codebase using the Grep or Glob tools, or run `grep -r` from your terminal.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Failover { .. } => Some(SlashCommandResult {
            message: "/failover is not yet implemented. Provider failover configuration is managed in settings.json under the providers section.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::GenerateImage { .. } => Some(SlashCommandResult {
            message: "/generate-image is not yet implemented. Image generation is not currently available in this session.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::HistoryArchive { .. } => Some(SlashCommandResult {
            // Phase 4.4: deprecation banner — `/history-archive` is a soft
            // alias for the L2 (episodic) view exposed by `/memory show
            // episodic`. Keep the original payload so existing scripts
            // don't change behavior; surface a one-line deprecation
            // warning on top.
            message: format!(
                "{}{}",
                phase4_4_deprecation_banner("/history-archive", "/memory show episodic"),
                "/history-archive is not yet implemented. Conversation archiving is not yet available. Export your terminal output manually to preserve a session.",
            ),
            session: session.clone(),
        }),
        SlashCommand::Configure { .. } => Some(SlashCommandResult {
            message: "/configure is not yet implemented. Edit your settings.json file directly to adjust Anvil's configuration.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Theme { .. } => Some(SlashCommandResult {
            message: "/theme is not yet implemented. Terminal color theming is not yet supported. Adjust your terminal emulator's color scheme instead.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::SemanticSearch { .. } => Some(SlashCommandResult {
            message: "/semantic-search is not yet implemented. Ask the assistant to search your documents semantically using the QMD tool or describe what you are looking for.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Docker { .. } => Some(SlashCommandResult {
            message: "/docker is not yet implemented. Run Docker commands directly from your terminal (e.g. `docker ps`, `docker compose up`), or ask the assistant to help compose a command.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Test { .. } => Some(SlashCommandResult {
            message: "/test is not yet implemented. Run your test suite directly from the terminal (e.g. `cargo test`, `npm test`), or ask the assistant to help write or debug tests.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Git { .. } => Some(SlashCommandResult {
            message: "/git is not yet implemented. Use git directly from your terminal. You can also ask the assistant to help with specific git operations like rebasing, cherry-picking, or resolving conflicts.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Refactor { .. } => Some(SlashCommandResult {
            message: "/refactor is not yet implemented. Describe the refactoring you want to the assistant — it can read your code, propose changes, and apply edits across multiple files.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Screenshot => Some(SlashCommandResult {
            message: "/screenshot is not yet implemented. Screenshot capture is not currently available in this terminal session.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Db { .. } => Some(SlashCommandResult {
            message: "/db is not yet implemented. Run database queries using your database client directly (e.g. `psql`, `mysql`), or ask the assistant to help write or explain SQL.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Security { .. } => Some(SlashCommandResult {
            message: "/security is not yet implemented. Ask the assistant to review your code for security issues, or run a dedicated scanner such as `cargo audit`, `npm audit`, or `bandit`.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Api { .. } => Some(SlashCommandResult {
            message: "/api is not yet implemented. Ask the assistant to help design, document, or test your API endpoints, or use a tool like `curl` or Postman to interact with them directly.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Docs { .. } => Some(SlashCommandResult {
            message: "/docs is not yet implemented. Ask the assistant to generate or update documentation for your code, or run your project's doc generation tool (e.g. `cargo doc`, `typedoc`).".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Scaffold { .. } => Some(SlashCommandResult {
            message: "/scaffold is not yet implemented. Ask the assistant to generate boilerplate code for a new module, component, or service — describe what you need and it will create the files.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Perf { .. } => Some(SlashCommandResult {
            message: "/perf is not yet implemented. Ask the assistant to analyze performance bottlenecks in your code, or use a profiler appropriate for your language (e.g. `perf`, `flamegraph`, `py-spy`).".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Debug { .. } => Some(SlashCommandResult {
            message: "/debug is not yet implemented. Describe the problem to the assistant and share relevant error output — it can trace through code, add logging, and suggest fixes.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Voice { .. } => Some(SlashCommandResult {
            message: "/voice is not yet implemented. Voice input is not available in this terminal session.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Collab { .. } => Some(SlashCommandResult {
            message: "/collab is not yet implemented. Real-time collaboration features are not yet available. Share your session output or use a shared terminal multiplexer (e.g. tmux) instead.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Changelog => Some(SlashCommandResult {
            message: "/changelog is not yet implemented. Run `git log --oneline` to see recent commits, or check the project's CHANGELOG.md file if one exists.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Env { .. } => Some(SlashCommandResult {
            message: "/env is not yet implemented. View or set environment variables directly in your shell (e.g. `env`, `export KEY=value`), or manage them via your project's .env file.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Hub { .. } => Some(SlashCommandResult {
            message: "/hub is not yet implemented. Use the `gh` CLI to interact with GitHub (e.g. `gh repo view`, `gh issue list`, `gh pr status`).".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Language { .. } => Some(SlashCommandResult {
            message: "/language is not yet implemented. Language-specific tooling runs through your existing build tools and linters — ask the assistant for help with a specific language task.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Lsp { .. } => Some(SlashCommandResult {
            message: "/lsp is not yet implemented. Language server integration is not yet available in Anvil. Use your editor's LSP support for diagnostics and completions.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Notebook { .. } => Some(SlashCommandResult {
            message: "/notebook is not yet implemented. Jupyter notebook editing is available via the NotebookEdit tool — ask the assistant to modify a notebook cell directly.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::K8s { .. } => Some(SlashCommandResult {
            message: "/k8s is not yet implemented. Use `kubectl` from your terminal to interact with Kubernetes clusters, or ask the assistant to help write manifests or debug deployments.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Iac { .. } => Some(SlashCommandResult {
            message: "/iac is not yet implemented. Run your infrastructure-as-code tools directly (e.g. `terraform plan`, `pulumi up`, `ansible-playbook`), or ask the assistant to help write or review IaC files.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Pipeline { .. } => Some(SlashCommandResult {
            message: "/pipeline is not yet implemented. Ask the assistant to help write or debug CI/CD pipeline configuration (GitHub Actions, GitLab CI, Jenkins, etc.).".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Review { .. } => Some(SlashCommandResult {
            message: "/review is not yet implemented. Ask the assistant to review your code — paste the relevant snippet or point it to a file, and it will provide feedback on correctness, style, and potential issues.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Deps { .. } => Some(SlashCommandResult {
            message: "/deps is not yet implemented. Check your dependencies using your package manager (e.g. `cargo tree`, `npm ls`, `pip list`), or ask the assistant to help audit or update them.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Mono { .. } => Some(SlashCommandResult {
            message: "/mono is not yet implemented. Monorepo management features are not yet available. Ask the assistant to help navigate or run cross-workspace commands in your monorepo.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Browser { .. } => Some(SlashCommandResult {
            message: "/browser is not yet implemented. Browser automation is not currently available. Ask the assistant to help write Playwright or Puppeteer scripts, or use the WebFetch tool for HTTP requests.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Notify { .. } => Some(SlashCommandResult {
            message: "/notify is not yet implemented. Notification delivery is not yet available in this session. Use external alerting tools or scripts to send messages to Slack, email, or other channels.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Vault { .. } => Some(SlashCommandResult {
            message: "/vault is not yet implemented. Manage secrets using your configured credential vault (e.g. HashiCorp Vault, AWS Secrets Manager, or the Anvil CVS service) directly from your terminal.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Migrate { .. } => Some(SlashCommandResult {
            message: "/migrate is not yet implemented. Run database migrations using your ORM's CLI (e.g. `prisma migrate dev`, `alembic upgrade head`, `rails db:migrate`), or ask the assistant to help write a migration.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Regex { .. } => Some(SlashCommandResult {
            message: "/regex is not yet implemented. Ask the assistant to write or explain a regular expression — describe the pattern you need to match and it will construct and test a regex for you.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Ssh { .. } => Some(SlashCommandResult {
            message: "/ssh is not yet implemented. Connect to remote hosts using `ssh` from your terminal. Ask the assistant to help configure SSH keys, jump hosts, or port forwarding.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Logs { .. } => Some(SlashCommandResult {
            message: "/logs is not yet implemented. View service logs using `journalctl`, `docker logs <container>`, `pm2 logs`, or your platform's log viewer. Ask the assistant to help parse or filter log output.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Markdown { .. } => Some(SlashCommandResult {
            message: "/markdown is not yet implemented. Ask the assistant to render, write, or convert Markdown content. For preview, use a Markdown viewer or your editor's preview mode.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Snippets { .. } => Some(SlashCommandResult {
            message: "/snippets is not yet implemented. Ask the assistant to generate reusable code snippets for a specific pattern or task. Save them to your editor's snippet library for future use.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Finetune { .. } => Some(SlashCommandResult {
            message: "/finetune is not yet implemented. Model fine-tuning is not available in this session. Use the Anthropic or OpenAI fine-tuning APIs directly if you need a custom model.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Webhook { .. } => Some(SlashCommandResult {
            message: "/webhook is not yet implemented. Configure webhooks through your platform's settings (GitHub, GitLab, etc.) or ask the assistant to help write a webhook handler.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Ssh { .. } => Some(SlashCommandResult {
            message: "/ssh: embedded SSH client. Run `anvil` interactively to use the SSH form, or `/ssh <alias>` to retrieve from the vault.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::PluginSdk { .. } => Some(SlashCommandResult {
            message: "/plugin-sdk is not yet implemented. Plugin development tooling is not yet available. Refer to the Anvil plugin documentation for the plugin manifest format and API.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Sleep => Some(SlashCommandResult {
            message: "/sleep is not yet implemented. This command would pause execution for a period of time. Use `sleep <seconds>` from your terminal if you need a delay in a script.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Think => Some(SlashCommandResult {
            message: "/think is not yet implemented. To prompt extended reasoning, ask the assistant to \"think step by step\" or \"reason through this carefully\" in your message.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Fast => Some(SlashCommandResult {
            message: "/fast is not yet implemented. Speed/quality tradeoff switching is not yet available. Select a faster model variant in your configuration if lower latency is needed.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::ReviewPr { .. } => Some(SlashCommandResult {
            message: "/review-pr is not yet implemented. Ask the assistant to review a pull request — provide the PR number or diff and it will analyze the changes for correctness, style, and potential issues.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::RemoteControl { .. } => Some(SlashCommandResult {
            message: "/remote-control is not yet implemented. Remote control features are managed through the Anvil web viewer. Start the web interface to access remote pairing and control.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Loop { .. } => Some(SlashCommandResult {
            message: "/loop is not yet implemented here. Use the loop skill (/loop) or ask the assistant to repeat a task with specific stopping criteria.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Focus => Some(SlashCommandResult {
            message: "/focus is not yet implemented. To narrow the assistant's attention to a specific file or task, mention it explicitly in your message or use /context (when available).".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Mcp { .. } => Some(SlashCommandResult {
            message: "/mcp is not yet implemented. MCP server management is configured at startup via settings.json. Restart Anvil after modifying MCP server entries.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Productivity => Some(SlashCommandResult {
            message: "/productivity is not yet implemented. Productivity metrics and session summaries are not yet available.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Knowledge { .. } => Some(SlashCommandResult {
            message: "/knowledge is handled by the runtime. Use /knowledge review, /knowledge accept <N>, or /knowledge reject <N>.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Daily { .. } => Some(SlashCommandResult {
            message: "/daily is handled by the runtime. Use /daily to view today's summary or /daily <date> for a specific day.".to_string(),
            session: session.clone(),
        }),
        // ── Ghost commands promoted in v2.2.6 ─────────────────────────────
        SlashCommand::Tab { .. } => Some(SlashCommandResult {
            message: "/tab is not yet implemented here. Tab management is available in TUI mode — use Ctrl+T (new), Ctrl+W (close), and Ctrl+] / Ctrl+[ (switch).".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Fork => Some(SlashCommandResult {
            message: "/fork is not yet implemented here. Tab forking is available in TUI mode — it duplicates the current tab with the same conversation context.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Share { .. } => Some(SlashCommandResult {
            message: "/share is not yet implemented. Tab sharing generates a read-only URL via the passage-culpur relay. This feature is available in TUI and web modes.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Audit => Some(SlashCommandResult {
            message: "/audit is not yet implemented. The composite audit runs /security scan + /deps audit + /vault verify in sequence. Run each individually in the meantime.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Restart { .. } => Some(SlashCommandResult {
            message: "/restart is not yet implemented here. In TUI mode, /restart will respawn the Anvil process (available in Phase 5).".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Agent { .. } => Some(SlashCommandResult {
            message: "/agent is not yet implemented here. Use /agent in the TUI to compose a temporary agent with traits.".to_string(),
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
            };
            Some(SlashCommandResult { message, session: session.clone() })
        }
        SlashCommand::Goal { action } => {
            let msg = match action.as_deref() {
                None | Some("list") => {
                    "/goal list — goal tracking is handled in the TUI. \
                     Use /goal new \"<description>\" to create a goal."
                        .to_string()
                }
                Some(other) => {
                    format!(
                        "/goal {other} — goal management is not yet implemented in non-TUI mode. \
                         Use /goal in an interactive session."
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
        SlashCommand::Unknown(cmd) => Some(SlashCommandResult {
            message: format!("/{cmd} is not a recognized command. Type /help to see all available commands."),
            session: session.clone(),
        }),
    }
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
                other     => format!(
                    "Unknown /memory subcommand: {other}\n\
                     Usage: /memory [show|inspect|promote|forget|why|budget|prune] [arg]"
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
/// 4. Egress allowlist (EgressPolicy::default — module ships but not yet
///    wired into central config; this view reports the runtime default so
///    users can see the current ground truth).
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

    // 4. Egress allowlist — the module ships with a default policy. It
    // is not yet wired into the central RuntimeFeatureConfig, so we
    // surface the runtime default with a clear note about that gap.
    lines.push(String::new());
    let policy = runtime::egress::EgressPolicy::default();
    let mut domains: Vec<_> = policy.allowlist.iter().cloned().collect();
    domains.sort();
    lines.push(format!(
        "egress allowlist (default policy, enabled={}, {} domain(s)):",
        policy.enabled,
        domains.len()
    ));
    for domain in domains.iter().take(20) {
        lines.push(format!("  allow  {domain}"));
    }
    if domains.len() > 20 {
        lines.push(format!("  ... +{} more", domains.len() - 20));
    }
    lines.push(
        "(egress policy is not yet merged into settings.json — defaults shown)"
            .to_string(),
    );

    lines.join("\n")
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
             /memory why reads ONLY the working-memory snapshot (L1) — it does NOT walk DailyStore (L2)\n\
             or reconcile across tiers. Use /memory show <tier> for tier-by-tier views."
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
  QMD context (when present)
  Configuration block
  <known-files> from L7 FileCacheManager (W11)
  Goal, Skill, FastMode/OutputStyle sections are layered on top

The vault, private memory, and encrypted tiers are NEVER injected automatically.
Nominations are SUGGESTED only -- they only enter the prompt after /memory promote.
/memory why reads ONLY the working-memory snapshot (L1) -- it does NOT walk DailyStore (L2)
or reconcile across tiers. Use /memory show <tier> for tier-by-tier views.
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

#[cfg(test)]
mod memory_tests {
    use super::*;

    #[test]
    fn memory_summary_contains_all_tiers() {
        let result = memory_summary();
        assert!(result.contains("anvil-md"), "should mention anvil-md tier");
        assert!(result.contains("nominations"), "should mention nominations tier");
        assert!(result.contains("daily"), "should mention daily tier");
        assert!(result.contains("vault"), "should mention vault tier");
    }

    #[test]
    fn memory_show_unknown_tier_returns_error() {
        let result = memory_show(Some("nonexistent-tier"), &MemoryContext::default());
        assert!(result.contains("Unknown tier"), "should report unknown tier");
    }

    #[test]
    fn memory_show_no_tier_returns_usage() {
        let result = memory_show(None, &MemoryContext::default());
        assert!(result.contains("Usage"), "should show usage when no tier given");
    }

    #[test]
    fn memory_show_vault_tier_returns_security_message() {
        let result = memory_show(Some("vault"), &MemoryContext::default());
        assert!(
            result.contains("security") || result.contains("encrypted"),
            "vault show should mention security/encryption"
        );
    }

    #[test]
    fn memory_show_private_tier_returns_security_message() {
        let result = memory_show(Some("private"), &MemoryContext::default());
        assert!(
            result.contains("encrypted") || result.contains("vault"),
            "private show should mention encryption"
        );
    }

    #[test]
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
    fn memory_inspect_empty_key_returns_usage() {
        let result = memory_inspect("");
        assert!(result.contains("Usage"), "should show usage for empty key");
    }

    #[test]
    fn memory_inspect_nonexistent_key_reports_not_found() {
        let result = memory_inspect("xyzzy_nonexistent_key_99999");
        assert!(result.contains("No entries"), "should report no entries found");
    }

    #[test]
    fn memory_promote_empty_id_returns_usage() {
        let result = memory_promote("");
        assert!(result.contains("Usage"), "should show usage for empty id");
    }

    #[test]
    fn memory_promote_unknown_id_reports_not_found() {
        let result = memory_promote("nom-xxx-not-real");
        assert!(
            result.contains("not found") || result.contains("Error"),
            "unknown id should report error; got: {result}"
        );
    }

    #[test]
    fn atomic_append_creates_file_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ANVIL.md");
        atomic_append_anvil_md(&path, "- hello world\n").expect("append");
        let contents = std::fs::read_to_string(&path).expect("read");
        assert!(contents.contains("- hello world"));
    }

    #[test]
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
    fn memory_forget_empty_key_returns_usage() {
        let result = memory_forget("");
        assert!(result.contains("Usage"), "should show usage for empty key");
    }

    #[test]
    fn memory_why_mentions_injection_order() {
        let result = memory_why(&MemoryContext::default());
        assert!(result.contains("System prompt"), "should describe system prompt");
        assert!(result.contains("ANVIL.md"), "should mention ANVIL.md");
    }

    #[test]
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
    fn memory_show_nominations_alias_carries_deprecation_banner() {
        // L3 §1 acceptance: the legacy top-level `nominations` tier is
        // kept for one cycle, but the output explains the rename so
        // users discover the new canonical form.
        let result = memory_show(Some("nominations"), &MemoryContext::default());
        assert!(
            result.contains("deprecated alias"),
            "should warn about the rename; got: {result}"
        );
        assert!(
            result.contains("semantic --pending"),
            "should advertise the new path; got: {result}"
        );
    }

    #[test]
    fn memory_show_semantic_unknown_sub_view_lists_known() {
        let result = memory_show(Some("semantic explode"), &MemoryContext::default());
        assert!(
            result.contains("Unknown semantic sub-view"),
            "should reject unknown; got: {result}"
        );
        assert!(result.contains("pending"));
    }

    #[test]
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
    fn memory_show_cache_file_routes_to_file_cache_manager() {
        let result = memory_show(Some("cache file"), &MemoryContext::default());
        assert!(
            result.contains("L7 Cache — file") || result.contains("not initialised"),
            "got: {result}"
        );
    }

    #[test]
    fn memory_show_cache_cmd_routes_to_cmd_cache_manager() {
        let result = memory_show(Some("cache cmd"), &MemoryContext::default());
        assert!(
            result.contains("L7 Cache — cmd") || result.contains("not initialised"),
            "got: {result}"
        );
    }

    #[test]
    fn memory_show_cache_qmd_routes_to_qmd_client() {
        // QMD binary may or may not be on PATH in CI. Either way, the
        // header should appear.
        let result = memory_show(Some("cache qmd"), &MemoryContext::default());
        assert!(result.contains("L7 Cache — qmd"), "got: {result}");
    }

    #[test]
    fn memory_show_cache_unknown_sub_view_lists_known() {
        let result = memory_show(Some("cache explode"), &MemoryContext::default());
        assert!(result.contains("Unknown cache sub-view"), "got: {result}");
    }

    #[test]
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
    fn memory_show_policy_shows_default_egress_status_line() {
        // L6 §5 acceptance: even when the egress block is not in
        // settings.json, the view reads EgressPolicy::default and tells
        // the user that the wiring gap exists.
        let result = memory_show(Some("policy"), &MemoryContext::default());
        assert!(
            result.contains("not yet merged into settings.json"),
            "should advertise the wiring gap; got: {result}"
        );
    }

    #[test]
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
    fn memory_show_procedural_goals_routes_to_goal_manager() {
        let result = memory_show(Some("procedural goals"), &MemoryContext::default());
        assert!(
            result.contains("L4 Procedural — goals"),
            "header missing; got: {result}"
        );
    }

    #[test]
    fn memory_show_procedural_cron_routes_to_cron_manager() {
        // L4 §3 acceptance: cron sub-view goes through CronManager::global.
        let result = memory_show(Some("procedural cron"), &MemoryContext::default());
        assert!(
            result.contains("L4 Procedural — cron"),
            "header missing; got: {result}"
        );
    }

    #[test]
    fn memory_show_procedural_unknown_sub_view_lists_known() {
        let result = memory_show(Some("procedural explode"), &MemoryContext::default());
        assert!(result.contains("Unknown procedural sub-view"), "got: {result}");
        assert!(result.contains("goals"));
        assert!(result.contains("routines"));
    }

    #[test]
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
    fn memory_summary_lists_episodic_tier() {
        // L2 §1 acceptance: `/memory` (no args) summary mentions episodic.
        let result = memory_summary();
        assert!(
            result.contains("episodic"),
            "summary should list episodic tier; got: {result}"
        );
    }

    #[test]
    fn memory_budget_lists_episodic_row() {
        // L2 §1 acceptance: `/memory budget` includes an episodic row.
        let result = memory_budget(&MemoryContext::default());
        assert!(
            result.contains("episodic"),
            "budget should include episodic row; got: {result}"
        );
    }

    #[test]
    fn memory_prune_returns_summary_with_both_tiers() {
        let result = memory_prune();
        assert!(result.contains("daily"), "should mention daily pruning");
        assert!(result.contains("nominations"), "should mention nominations pruning");
    }

    #[test]
    fn handle_memory_command_none_dispatches_to_summary() {
        let result = handle_memory_command(None, &MemoryContext::default());
        assert!(result.contains("anvil-md"), "summary should contain tier info");
    }

    #[test]
    fn handle_memory_command_dispatches_subcommands() {
        let why = handle_memory_command(Some("why"), &MemoryContext::default());
        assert!(why.contains("System prompt"), "why should describe injection");

        let budget = handle_memory_command(Some("budget"), &MemoryContext::default());
        assert!(budget.contains("TOTAL"), "budget should show totals");

        let unknown = handle_memory_command(Some("explode"), &MemoryContext::default());
        assert!(unknown.contains("Unknown"), "unknown subcommand should error");
    }
}
