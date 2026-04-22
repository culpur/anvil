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
        SlashCommand::Memory => Some(SlashCommandResult {
            message: "/memory is not yet implemented. Memory files (CLAUDE.md, MEMORY.md) are loaded automatically at session start — edit them directly to update persistent context.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Init => Some(SlashCommandResult {
            message: "/init is not yet implemented. To initialize Anvil in a new project, create a CLAUDE.md file in your project root with instructions for the assistant.".to_string(),
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
            message: "/pin is not yet implemented. To keep important context available, add it to your CLAUDE.md file so it is loaded at every session start.".to_string(),
            session: session.clone(),
        }),
        SlashCommand::Unpin { .. } => Some(SlashCommandResult {
            message: "/unpin is not yet implemented. Remove entries from your CLAUDE.md file to stop including them in future sessions.".to_string(),
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
        SlashCommand::Doctor => Some(SlashCommandResult {
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
            };
            Some(SlashCommandResult { message, session: session.clone() })
        }
        SlashCommand::Unknown(cmd) => Some(SlashCommandResult {
            message: format!("/{cmd} is not a recognized command. Type /help to see all available commands."),
            session: session.clone(),
        }),
    }
}
