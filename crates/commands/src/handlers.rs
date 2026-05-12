use runtime::{compact_session, CompactionConfig, Session};

use crate::specs::{render_command_detailed_help, render_slash_command_help};
use crate::SlashCommand;

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
            message: handle_memory_command(action.as_deref()),
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
            message: "/history-archive is not yet implemented. Conversation archiving is not yet available. Export your terminal output manually to preserve a session.".to_string(),
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
            message: "/file-cache: use /file-cache list, stats, prune, or forget <path>".to_string(),
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

fn handle_memory_command(action: Option<&str>) -> String {
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
                "show"    => memory_show(if arg.is_empty() { None } else { Some(arg) }),
                "inspect" => memory_inspect(arg),
                "promote" => memory_promote(arg),
                "forget"  => memory_forget(arg),
                "why"     => memory_why(),
                "budget"  => memory_budget(),
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
    let home = anvil_home();
    let mut lines = vec!["Memory tier summary:".to_string()];
    let md_count = count_files_with_ext(&home.join("memory"), "md");
    lines.push(format!("  anvil-md     {} file(s) in ~/.anvil/memory/", md_count));
    let nom_count = count_files_with_ext(&home.join("nominations"), "json");
    lines.push(format!("  nominations   {} pending nomination(s)", nom_count));
    let daily_count = count_files_with_ext(&home.join("daily"), "json");
    lines.push(format!("  daily         {} session file(s)", daily_count));
    let goals_count = count_files_with_ext(&home.join("goals"), "json");
    lines.push(format!("  goals         {} goal file(s)", goals_count));
    if home.join("vault.bin").exists() {
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
    let fc_count = count_files_with_ext(&home.join("file-cache"), "json");
    lines.push(format!("  file-cache    {} cache entries", fc_count));
    let cc_count = count_files_with_ext(&home.join("cmd-cache"), "json");
    lines.push(format!("  cmd-cache     {} cache entries", cc_count));
    lines.push(String::new());
    lines.push("Use /memory show <tier> to inspect a specific tier.".to_string());
    lines.join("\n")
}

fn memory_show(tier: Option<&str>) -> String {
    use runtime::{DailyStore, GoalManager, MemoryManager};

    let tier = match tier {
        Some(t) => t,
        None => {
            return "Usage: /memory show <tier>\n\
                Tiers: anvil-md, vault, private, nominations, daily, file-cache, cmd-cache, goals"
                .to_string()
        }
    };

    let home = anvil_home();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    match tier {
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
            let store =
                runtime::nominations::NominationStore::with_dir(home.join("nominations"));
            store.format_pending()
        }
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
             Known tiers: anvil-md, vault, private, nominations, daily, file-cache, cmd-cache, goals"
        ),
    }
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

fn memory_promote(id: &str) -> String {
    if id.is_empty() {
        return "Usage: /memory promote <nomination-id>".to_string();
    }
    let store =
        runtime::nominations::NominationStore::with_dir(anvil_home().join("nominations"));
    match store.accept(id, "ANVIL.md") {
        Ok(()) => format!(
            "Nomination '{id}' accepted and marked for promotion into ANVIL.md."
        ),
        Err(e) => format!("Error promoting nomination '{id}': {e}"),
    }
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

fn memory_why() -> String {
    "\
System prompt injection order for this session:

  1. Base system prompt (hardcoded assistant instructions)
  2. ANVIL.md files (project root, then ~/.anvil/memory/*.md)
  3. Active goal fragment (if a goal is active via /goal)
  4. Skill body (if a skill was loaded via /skill load)
  5. File-cache known-files block (compact per-file summaries, W11)
  6. Daily task reconciliation fragment (pending tasks from yesterday)

The vault, private memory, and encrypted tiers are NEVER injected automatically.
Nominations are SUGGESTED only -- they only enter the prompt after /memory promote.
"
    .to_string()
}

fn memory_budget() -> String {
    let home = anvil_home();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let tiers: &[(&str, std::path::PathBuf)] = &[
        ("anvil-md", cwd),
        ("nominations", home.join("nominations")),
        ("daily", home.join("daily")),
        ("goals", home.join("goals")),
        ("file-cache", home.join("file-cache")),
        ("cmd-cache", home.join("cmd-cache")),
    ];

    let mut lines = vec!["Memory tier token budget (approximate):".to_string()];
    lines.push(format!("  {:<14}  {:>10}  {:>12}", "Tier", "Bytes", "~Tokens"));
    lines.push(format!("  {}", "-".repeat(40)));

    let mut grand_bytes = 0u64;
    for (name, dir) in tiers {
        let bytes = dir_total_bytes(dir);
        grand_bytes += bytes;
        let tokens = bytes / 4;
        lines.push(format!("  {name:<14}  {bytes:>10}  {tokens:>12}"));
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
        let result = memory_show(Some("nonexistent-tier"));
        assert!(result.contains("Unknown tier"), "should report unknown tier");
    }

    #[test]
    fn memory_show_no_tier_returns_usage() {
        let result = memory_show(None);
        assert!(result.contains("Usage"), "should show usage when no tier given");
    }

    #[test]
    fn memory_show_vault_tier_returns_security_message() {
        let result = memory_show(Some("vault"));
        assert!(
            result.contains("security") || result.contains("encrypted"),
            "vault show should mention security/encryption"
        );
    }

    #[test]
    fn memory_show_private_tier_returns_security_message() {
        let result = memory_show(Some("private"));
        assert!(
            result.contains("encrypted") || result.contains("vault"),
            "private show should mention encryption"
        );
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
    fn memory_forget_empty_key_returns_usage() {
        let result = memory_forget("");
        assert!(result.contains("Usage"), "should show usage for empty key");
    }

    #[test]
    fn memory_why_mentions_injection_order() {
        let result = memory_why();
        assert!(result.contains("system prompt"), "should describe system prompt");
        assert!(result.contains("ANVIL.md"), "should mention ANVIL.md");
    }

    #[test]
    fn memory_budget_shows_tiers_and_totals() {
        let result = memory_budget();
        assert!(result.contains("anvil-md"), "should show anvil-md tier");
        assert!(result.contains("TOTAL"), "should show total row");
        assert!(
            result.contains("Tokens") || result.contains("token"),
            "should show token estimate"
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
        let result = handle_memory_command(None);
        assert!(result.contains("anvil-md"), "summary should contain tier info");
    }

    #[test]
    fn handle_memory_command_dispatches_subcommands() {
        let why = handle_memory_command(Some("why"));
        assert!(why.contains("system prompt"), "why should describe injection");

        let budget = handle_memory_command(Some("budget"));
        assert!(budget.contains("TOTAL"), "budget should show totals");

        let unknown = handle_memory_command(Some("explode"));
        assert!(unknown.contains("Unknown"), "unknown subcommand should error");
    }
}
