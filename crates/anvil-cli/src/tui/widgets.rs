/// Completion widget data: slash command lists, Ollama model cache, clipboard.
use super::state::{CompletionItem, CompletionPopup};

// ─── Slash command completions ────────────────────────────────────────────────

/// All top-level slash commands with short descriptions.
pub(super) fn all_slash_commands() -> Vec<CompletionItem> {
    vec![
        CompletionItem { insert: "/help".into(), hint: "Show available commands".into() },
        CompletionItem { insert: "/status".into(), hint: "Session + workspace status".into() },
        CompletionItem { insert: "/model".into(), hint: "Show or switch model".into() },
        CompletionItem { insert: "/provider".into(), hint: "Switch provider (anthropic/openai/ollama)".into() },
        CompletionItem { insert: "/login".into(), hint: "Login/refresh provider credentials".into() },
        CompletionItem { insert: "/compact".into(), hint: "Compact session history".into() },
        CompletionItem { insert: "/clear".into(), hint: "Start a fresh session".into() },
        CompletionItem { insert: "/cost".into(), hint: "Token usage for this session".into() },
        CompletionItem { insert: "/tokens".into(), hint: "Detailed token breakdown".into() },
        CompletionItem { insert: "/diff".into(), hint: "Show git diff".into() },
        CompletionItem { insert: "/version".into(), hint: "CLI version info".into() },
        CompletionItem { insert: "/memory".into(), hint: "Loaded memory files".into() },
        CompletionItem { insert: "/config".into(), hint: "Inspect configuration".into() },
        CompletionItem { insert: "/export".into(), hint: "Export conversation".into() },
        CompletionItem { insert: "/session".into(), hint: "List or switch sessions".into() },
        CompletionItem { insert: "/permissions".into(), hint: "Show or set permission mode".into() },
        CompletionItem { insert: "/init".into(), hint: "Scaffold ANVIL.md + config".into() },
        CompletionItem { insert: "/commit".into(), hint: "Generate commit message + commit".into() },
        CompletionItem { insert: "/commit-push-pr".into(), hint: "Commit, push, and open PR".into() },
        CompletionItem { insert: "/pr".into(), hint: "Draft or create a pull request".into() },
        CompletionItem { insert: "/issue".into(), hint: "Draft or create a GitHub issue".into() },
        CompletionItem { insert: "/branch".into(), hint: "List, create, or switch branches".into() },
        CompletionItem { insert: "/worktree".into(), hint: "Manage git worktrees".into() },
        CompletionItem { insert: "/bughunter".into(), hint: "Scan codebase for bugs".into() },
        CompletionItem { insert: "/ultraplan".into(), hint: "Deep planning with reasoning".into() },
        CompletionItem { insert: "/teleport".into(), hint: "Jump to a file or symbol".into() },
        CompletionItem { insert: "/qmd".into(), hint: "Search knowledge base".into() },
        CompletionItem { insert: "/doctor".into(), hint: "Diagnose configuration".into() },
        CompletionItem { insert: "/context".into(), hint: "Add file to context".into() },
        CompletionItem { insert: "/pin".into(), hint: "Pin file to always-in-context".into() },
        CompletionItem { insert: "/unpin".into(), hint: "Remove pinned file".into() },
        CompletionItem { insert: "/undo".into(), hint: "Undo last file changes".into() },
        CompletionItem { insert: "/history".into(), hint: "Show conversation history".into() },
        CompletionItem { insert: "/chat".into(), hint: "Toggle chat-only mode (no tools)".into() },
        CompletionItem { insert: "/vim".into(), hint: "Toggle vim keybindings".into() },
        CompletionItem { insert: "/web".into(), hint: "Quick web search".into() },
        CompletionItem { insert: "/agents".into(), hint: "List configured agents".into() },
        CompletionItem { insert: "/skills".into(), hint: "List available skills".into() },
        CompletionItem { insert: "/plugins".into(), hint: "Manage plugins".into() },
        CompletionItem { insert: "/debug-tool-call".into(), hint: "Replay last tool call".into() },
        CompletionItem { insert: "/configure".into(), hint: "Interactive configuration menu".into() },
        CompletionItem { insert: "/theme".into(), hint: "Switch terminal color theme".into() },
        CompletionItem { insert: "/search".into(), hint: "Multi-provider web search".into() },
        CompletionItem { insert: "/failover".into(), hint: "AI provider failover chain".into() },
        CompletionItem { insert: "/image".into(), hint: "Generate image via GPT Image".into() },
        CompletionItem { insert: "/history-archive".into(), hint: "Browse archived sessions".into() },
        CompletionItem { insert: "/exit".into(), hint: "Exit Anvil".into() },
        CompletionItem { insert: "/tab".into(), hint: "Manage tabs (new/close/list/rename)".into() },
        CompletionItem { insert: "/semantic-search".into(), hint: "Search symbols grouped by type".into() },
        CompletionItem { insert: "/docker".into(), hint: "Docker container & compose helpers".into() },
        CompletionItem { insert: "/test".into(), hint: "Generate, run, or show test coverage".into() },
        CompletionItem { insert: "/git".into(), hint: "Advanced git: rebase, conflicts, stash".into() },
        CompletionItem { insert: "/refactor".into(), hint: "Rename, extract, or move code".into() },
        CompletionItem { insert: "/screenshot".into(), hint: "Capture screen and send to AI".into() },
        CompletionItem { insert: "/db".into(), hint: "Database: connect, schema, query, migrate".into() },
        CompletionItem { insert: "/security".into(), hint: "Security: scan, secrets, deps, report".into() },
        CompletionItem { insert: "/api".into(), hint: "API: spec, mock, test, docs".into() },
        CompletionItem { insert: "/docs".into(), hint: "Docs: generate, readme, architecture, changelog".into() },
        CompletionItem { insert: "/hub".into(), hint: "Browse AnvilHub marketplace".into() },
        CompletionItem { insert: "/language".into(), hint: "Set display language (en, de, es, fr, ja, zh-CN, ru)".into() },
        CompletionItem { insert: "/scaffold".into(), hint: "Scaffold a new project from a template".into() },
        CompletionItem { insert: "/perf".into(), hint: "Performance profiling and benchmarking".into() },
        CompletionItem { insert: "/debug".into(), hint: "Debugging helpers: start, breakpoint, watch, explain".into() },
        CompletionItem { insert: "/voice".into(), hint: "Voice input (coming soon)".into() },
        CompletionItem { insert: "/collab".into(), hint: "Collaborative session sharing (coming soon)".into() },
        CompletionItem { insert: "/changelog".into(), hint: "Generate CHANGELOG.md entry from git log".into() },
        CompletionItem { insert: "/env".into(), hint: "Manage session environment variables".into() },
        CompletionItem { insert: "/vault".into(), hint: "Encrypted credential vault and TOTP manager".into() },
        CompletionItem { insert: "/lsp".into(), hint: "LSP: start server, list symbols, find references".into() },
        CompletionItem { insert: "/notebook".into(), hint: "Jupyter notebook: run, cell, export".into() },
        CompletionItem { insert: "/k8s".into(), hint: "Kubernetes: pods, logs, apply, describe".into() },
        CompletionItem { insert: "/iac".into(), hint: "IaC: plan, apply, validate, drift (terraform/tofu)".into() },
        CompletionItem { insert: "/pipeline".into(), hint: "CI/CD: generate, lint, or run pipeline".into() },
        CompletionItem { insert: "/review".into(), hint: "Code review: file, staged changes, or PR diff".into() },
        CompletionItem { insert: "/deps".into(), hint: "Dependencies: tree, outdated, audit, why".into() },
        CompletionItem { insert: "/mono".into(), hint: "Monorepo: list, graph, changed, run".into() },
        CompletionItem { insert: "/browser".into(), hint: "Browser: open URL, screenshot, test".into() },
        CompletionItem { insert: "/notify".into(), hint: "Notifications: desktop, webhook, Matrix, Discord, Slack, Telegram, WhatsApp, Signal".into() },
        CompletionItem { insert: "/migrate".into(), hint: "Migrate framework, language, or dependency manager".into() },
        CompletionItem { insert: "/regex".into(), hint: "Regex: build, test, or explain a pattern".into() },
        CompletionItem { insert: "/ssh".into(), hint: "SSH: list hosts, connect, tunnel, or list keys".into() },
        CompletionItem { insert: "/logs".into(), hint: "Logs: tail, search, analyze, or stats".into() },
        CompletionItem { insert: "/markdown".into(), hint: "Markdown: preview, table of contents, or lint".into() },
        CompletionItem { insert: "/snippets".into(), hint: "Snippets: save, list, get, or search".into() },
        CompletionItem { insert: "/finetune".into(), hint: "Fine-tuning: prepare data, validate, start job, status".into() },
        CompletionItem { insert: "/webhook".into(), hint: "Webhooks: list, add, test, or remove endpoints".into() },
        CompletionItem { insert: "/plugin-sdk".into(), hint: "Plugin SDK: init, build, test, or publish".into() },
    ]
}

/// Sub-command completions for commands that have them.
pub(super) fn subcommands_for(command: &str) -> Vec<CompletionItem> {
    match command {
        "/provider" | "/providers" => vec![
            CompletionItem { insert: "list".into(), hint: "List models for current provider".into() },
            CompletionItem { insert: "anthropic".into(), hint: "Switch to Anthropic (Claude)".into() },
            CompletionItem { insert: "openai".into(), hint: "Switch to OpenAI (GPT)".into() },
            CompletionItem { insert: "ollama".into(), hint: "Switch to Ollama (local)".into() },
            CompletionItem { insert: "xai".into(), hint: "Switch to xAI (Grok)".into() },
            CompletionItem { insert: "login".into(), hint: "Login/refresh current provider".into() },
        ],
        "/login" => vec![
            CompletionItem { insert: "anthropic".into(), hint: "Login to Anthropic (OAuth)".into() },
            CompletionItem { insert: "openai".into(), hint: "Setup OpenAI API key".into() },
            CompletionItem { insert: "ollama".into(), hint: "Configure Ollama endpoint".into() },
        ],
        "/config" => vec![
            CompletionItem { insert: "env".into(), hint: "Show environment config".into() },
            CompletionItem { insert: "hooks".into(), hint: "Show hook config".into() },
            CompletionItem { insert: "model".into(), hint: "Show model config".into() },
            CompletionItem { insert: "plugins".into(), hint: "Show plugin config".into() },
        ],
        "/permissions" => vec![
            CompletionItem { insert: "read-only".into(), hint: "Read-only access".into() },
            CompletionItem { insert: "workspace-write".into(), hint: "Write within workspace".into() },
            CompletionItem { insert: "danger-full-access".into(), hint: "Full access (no restrictions)".into() },
        ],
        "/session" => vec![
            CompletionItem { insert: "list".into(), hint: "List saved sessions".into() },
            CompletionItem { insert: "switch".into(), hint: "Switch to a session".into() },
        ],
        "/branch" => vec![
            CompletionItem { insert: "list".into(), hint: "List branches".into() },
            CompletionItem { insert: "create".into(), hint: "Create a new branch".into() },
            CompletionItem { insert: "switch".into(), hint: "Switch to a branch".into() },
        ],
        "/worktree" => vec![
            CompletionItem { insert: "list".into(), hint: "List worktrees".into() },
            CompletionItem { insert: "add".into(), hint: "Add a worktree".into() },
            CompletionItem { insert: "remove".into(), hint: "Remove a worktree".into() },
            CompletionItem { insert: "prune".into(), hint: "Prune stale worktrees".into() },
        ],
        "/plugins" | "/plugin" => vec![
            CompletionItem { insert: "list".into(), hint: "List installed plugins".into() },
            CompletionItem { insert: "install".into(), hint: "Install a plugin".into() },
            CompletionItem { insert: "enable".into(), hint: "Enable a plugin".into() },
            CompletionItem { insert: "disable".into(), hint: "Disable a plugin".into() },
            CompletionItem { insert: "uninstall".into(), hint: "Uninstall a plugin".into() },
        ],
        "/tab" => vec![
            CompletionItem { insert: "new".into(), hint: "Open a new tab".into() },
            CompletionItem { insert: "close".into(), hint: "Close the current tab".into() },
            CompletionItem { insert: "list".into(), hint: "List all open tabs".into() },
            CompletionItem { insert: "rename".into(), hint: "Rename the current tab".into() },
        ],
        "/clear" => vec![
            CompletionItem { insert: "--confirm".into(), hint: "Confirm session clear".into() },
        ],
        "/history" => vec![
            CompletionItem { insert: "all".into(), hint: "Show full history".into() },
        ],
        "/configure" | "/settings" => vec![
            CompletionItem { insert: "providers".into(), hint: "Provider & auth settings".into() },
            CompletionItem { insert: "models".into(), hint: "Default model & failover".into() },
            CompletionItem { insert: "context".into(), hint: "Context, memory, QMD".into() },
            CompletionItem { insert: "search".into(), hint: "Search provider keys".into() },
            CompletionItem { insert: "permissions".into(), hint: "Permission mode".into() },
            CompletionItem { insert: "display".into(), hint: "Vim, chat, theme".into() },
            CompletionItem { insert: "integrations".into(), hint: "AnvilHub, WordPress, GitHub".into() },
            CompletionItem { insert: "vault".into(), hint: "Credential vault settings".into() },
            CompletionItem { insert: "notifications".into(), hint: "Notification channels (Matrix, Discord…)".into() },
            CompletionItem { insert: "failover".into(), hint: "Model failover chain & cooldowns".into() },
            CompletionItem { insert: "ssh".into(), hint: "SSH key, bastion & config path".into() },
            CompletionItem { insert: "dockerk8s".into(), hint: "Docker Compose & Kubernetes settings".into() },
            CompletionItem { insert: "database".into(), hint: "Database connection settings".into() },
            CompletionItem { insert: "memoryarchive".into(), hint: "Memory dir & archive settings".into() },
            CompletionItem { insert: "pluginscron".into(), hint: "Plugin search paths & cron jobs".into() },
        ],
        "/theme" => vec![
            CompletionItem { insert: "list".into(), hint: "Show available themes".into() },
            CompletionItem { insert: "set".into(), hint: "Apply a theme".into() },
            CompletionItem { insert: "reset".into(), hint: "Reset to default (culpur-defense)".into() },
            CompletionItem { insert: "create".into(), hint: "Create a new custom theme".into() },
            CompletionItem { insert: "import".into(), hint: "Import theme from a JSON file".into() },
            CompletionItem { insert: "export".into(), hint: "Export current theme to a JSON file".into() },
            CompletionItem { insert: "cyberpunk".into(), hint: "Neon pink + electric blue".into() },
            CompletionItem { insert: "nord".into(), hint: "Arctic blues + muted greens".into() },
            CompletionItem { insert: "solarized-dark".into(), hint: "Classic calibrated colors".into() },
            CompletionItem { insert: "dracula".into(), hint: "Purple + pink + green".into() },
            CompletionItem { insert: "culpur-defense".into(), hint: "Navy + cyan (default)".into() },
            CompletionItem { insert: "monokai".into(), hint: "Gold + green + magenta classic".into() },
            CompletionItem { insert: "gruvbox".into(), hint: "Warm retro browns + gold".into() },
            CompletionItem { insert: "catppuccin".into(), hint: "Pastel lavender + pink".into() },
        ],
        "/search" => vec![
            CompletionItem { insert: "providers".into(), hint: "List search providers".into() },
        ],
        "/failover" => vec![
            CompletionItem { insert: "status".into(), hint: "Show failover chain status".into() },
            CompletionItem { insert: "add".into(), hint: "Add model to chain".into() },
            CompletionItem { insert: "remove".into(), hint: "Remove model from chain".into() },
            CompletionItem { insert: "reset".into(), hint: "Clear all cooldowns".into() },
        ],
        "/model" => {
            let mut models = vec![
                CompletionItem { insert: "claude-opus-4-6".into(), hint: "Anthropic Opus (most capable)".into() },
                CompletionItem { insert: "claude-sonnet-4-6".into(), hint: "Anthropic Sonnet (balanced)".into() },
                CompletionItem { insert: "claude-haiku-4-5".into(), hint: "Anthropic Haiku (fast)".into() },
                CompletionItem { insert: "gpt-5.4-mini".into(), hint: "OpenAI GPT-5.4 Mini".into() },
                CompletionItem { insert: "gpt-5.4".into(), hint: "OpenAI GPT-5.4 (flagship)".into() },
                CompletionItem { insert: "gpt-5".into(), hint: "OpenAI GPT-5".into() },
                CompletionItem { insert: "o3".into(), hint: "OpenAI o3 (reasoning)".into() },
                CompletionItem { insert: "grok".into(), hint: "xAI Grok".into() },
            ];
            models.extend(cached_ollama_models()
                .into_iter()
                .map(|(name, size)| CompletionItem {
                    insert: name,
                    hint: format!("Ollama local ({size})"),
                }));
            models
        },
        "/image" | "/generate-image" => vec![
            CompletionItem { insert: "--wp".into(), hint: "Upload to WordPress as featured image".into() },
        ],
        "/history-archive" => vec![
            CompletionItem { insert: "search".into(), hint: "Search archived sessions".into() },
            CompletionItem { insert: "view".into(), hint: "View a specific archive".into() },
        ],
        "/semantic-search" | "/symsearch" => vec![
            CompletionItem { insert: "--type fn".into(), hint: "Filter to function definitions".into() },
            CompletionItem { insert: "--type class".into(), hint: "Filter to class definitions".into() },
            CompletionItem { insert: "--type struct".into(), hint: "Filter to struct definitions".into() },
            CompletionItem { insert: "--type import".into(), hint: "Filter to import statements".into() },
            CompletionItem { insert: "--lang rs".into(), hint: "Limit to Rust files".into() },
            CompletionItem { insert: "--lang ts".into(), hint: "Limit to TypeScript files".into() },
            CompletionItem { insert: "--lang py".into(), hint: "Limit to Python files".into() },
        ],
        "/docker" => vec![
            CompletionItem { insert: "ps".into(), hint: "List running containers".into() },
            CompletionItem { insert: "logs".into(), hint: "Show container logs".into() },
            CompletionItem { insert: "compose".into(), hint: "Show docker-compose services".into() },
            CompletionItem { insert: "build".into(), hint: "Build from Dockerfile in cwd".into() },
        ],
        "/test" => vec![
            CompletionItem { insert: "generate".into(), hint: "Generate unit tests for a file".into() },
            CompletionItem { insert: "run".into(), hint: "Run the project test suite".into() },
            CompletionItem { insert: "coverage".into(), hint: "Show test coverage summary".into() },
        ],
        "/git" => vec![
            CompletionItem { insert: "rebase".into(), hint: "Interactive rebase assistant".into() },
            CompletionItem { insert: "conflicts".into(), hint: "Detect and resolve merge conflicts".into() },
            CompletionItem { insert: "cherry-pick".into(), hint: "Cherry-pick commit assistant".into() },
            CompletionItem { insert: "stash".into(), hint: "Stash management".into() },
        ],
        "/refactor" => vec![
            CompletionItem { insert: "rename".into(), hint: "Rename symbol across codebase".into() },
            CompletionItem { insert: "extract".into(), hint: "Extract lines to a new function".into() },
            CompletionItem { insert: "move".into(), hint: "Move code between files".into() },
        ],
        "/db" => vec![
            CompletionItem { insert: "connect".into(), hint: "Connect to a database URL".into() },
            CompletionItem { insert: "schema".into(), hint: "Inspect schema files in project".into() },
            CompletionItem { insert: "query".into(), hint: "Analyse a SQL query with AI".into() },
            CompletionItem { insert: "migrate".into(), hint: "Detect drift & suggest migrations".into() },
        ],
        "/security" => vec![
            CompletionItem { insert: "scan".into(), hint: "Grep for vulnerability patterns".into() },
            CompletionItem { insert: "secrets".into(), hint: "Detect hardcoded secrets".into() },
            CompletionItem { insert: "deps".into(), hint: "Check dependencies for CVEs".into() },
            CompletionItem { insert: "report".into(), hint: "Generate combined security report".into() },
        ],
        "/api" => vec![
            CompletionItem { insert: "spec".into(), hint: "Generate OpenAPI spec from file".into() },
            CompletionItem { insert: "mock".into(), hint: "Start mock server from spec".into() },
            CompletionItem { insert: "test".into(), hint: "Test an endpoint via curl".into() },
            CompletionItem { insert: "docs".into(), hint: "Generate API documentation".into() },
        ],
        "/docs" => vec![
            CompletionItem { insert: "generate".into(), hint: "Auto-generate project docs".into() },
            CompletionItem { insert: "readme".into(), hint: "Generate or update README.md".into() },
            CompletionItem { insert: "architecture".into(), hint: "Generate architecture description".into() },
            CompletionItem { insert: "changelog".into(), hint: "Generate changelog from git log".into() },
        ],
        "/hub" => vec![
            CompletionItem { insert: "search".into(), hint: "Search packages by keyword".into() },
            CompletionItem { insert: "skills".into(), hint: "Browse top community skills".into() },
            CompletionItem { insert: "plugins".into(), hint: "Browse top plugins".into() },
            CompletionItem { insert: "agents".into(), hint: "Browse top agents".into() },
            CompletionItem { insert: "themes".into(), hint: "Browse top themes".into() },
            CompletionItem { insert: "install".into(), hint: "Install a package by name".into() },
            CompletionItem { insert: "info".into(), hint: "Show package details".into() },
        ],
        "/language" | "/lang" => vec![
            CompletionItem { insert: "en".into(), hint: "English (default)".into() },
            CompletionItem { insert: "de".into(), hint: "Deutsch — German".into() },
            CompletionItem { insert: "es".into(), hint: "Español — Spanish".into() },
            CompletionItem { insert: "fr".into(), hint: "Français — French".into() },
            CompletionItem { insert: "ja".into(), hint: "日本語 — Japanese".into() },
            CompletionItem { insert: "zh-CN".into(), hint: "简体中文 — Chinese Simplified".into() },
            CompletionItem { insert: "ru".into(), hint: "Русский — Russian".into() },
        ],
        "/scaffold" => vec![
            CompletionItem { insert: "new".into(), hint: "Create a new project from a template".into() },
            CompletionItem { insert: "list".into(), hint: "List available project templates".into() },
        ],
        "/perf" => vec![
            CompletionItem { insert: "profile".into(), hint: "Profile a command and show wall-time".into() },
            CompletionItem { insert: "benchmark".into(), hint: "Benchmark functions in a file".into() },
            CompletionItem { insert: "flamegraph".into(), hint: "Generate a flamegraph with cargo-flamegraph".into() },
            CompletionItem { insert: "analyze".into(), hint: "AI-assisted performance analysis".into() },
        ],
        "/debug" => vec![
            CompletionItem { insert: "start".into(), hint: "Start debugger for a file".into() },
            CompletionItem { insert: "breakpoint".into(), hint: "Set a breakpoint at file:line".into() },
            CompletionItem { insert: "watch".into(), hint: "Watch an expression".into() },
            CompletionItem { insert: "explain".into(), hint: "Explain an error message".into() },
        ],
        "/voice" => vec![
            CompletionItem { insert: "start".into(), hint: "Start microphone capture (coming soon)".into() },
            CompletionItem { insert: "stop".into(), hint: "Stop microphone capture (coming soon)".into() },
        ],
        "/collab" => vec![
            CompletionItem { insert: "share".into(), hint: "Share this session (coming soon)".into() },
            CompletionItem { insert: "join".into(), hint: "Join a shared session by ID (coming soon)".into() },
        ],
        "/env" => vec![
            CompletionItem { insert: "show".into(), hint: "Show current environment variables".into() },
            CompletionItem { insert: "set".into(), hint: "Set an environment variable".into() },
            CompletionItem { insert: "load".into(), hint: "Load variables from a .env file".into() },
            CompletionItem { insert: "diff".into(), hint: "Diff current env against a .env file".into() },
        ],
        "/vault" => vec![
            CompletionItem { insert: "setup".into(), hint: "Initialize vault with master password".into() },
            CompletionItem { insert: "unlock".into(), hint: "Unlock vault (provide master password)".into() },
            CompletionItem { insert: "lock".into(), hint: "Lock vault and clear KEK from memory".into() },
            CompletionItem { insert: "store".into(), hint: "Store an encrypted credential".into() },
            CompletionItem { insert: "get".into(), hint: "Decrypt and display a credential".into() },
            CompletionItem { insert: "list".into(), hint: "List stored credential labels".into() },
            CompletionItem { insert: "delete".into(), hint: "Delete a credential".into() },
            CompletionItem { insert: "totp".into(), hint: "TOTP sub-commands (add/generate/list/delete)".into() },
        ],
        "/lsp" => vec![
            CompletionItem { insert: "start".into(), hint: "Start language server for a language".into() },
            CompletionItem { insert: "symbols".into(), hint: "List symbols in a file via LSP".into() },
            CompletionItem { insert: "references".into(), hint: "Find all references to a symbol".into() },
        ],
        "/notebook" => vec![
            CompletionItem { insert: "run".into(), hint: "Execute all cells in a .ipynb notebook".into() },
            CompletionItem { insert: "cell".into(), hint: "Run specific cell by index".into() },
            CompletionItem { insert: "export".into(), hint: "Export notebook to html/py/pdf".into() },
        ],
        "/k8s" | "/kubectl" => vec![
            CompletionItem { insert: "pods".into(), hint: "List pods in current namespace".into() },
            CompletionItem { insert: "logs".into(), hint: "Tail last 50 lines of pod logs".into() },
            CompletionItem { insert: "apply".into(), hint: "Apply a manifest file".into() },
            CompletionItem { insert: "describe".into(), hint: "Describe a resource".into() },
        ],
        "/iac" | "/terraform" => vec![
            CompletionItem { insert: "plan".into(), hint: "Run terraform/tofu plan".into() },
            CompletionItem { insert: "apply".into(), hint: "Run terraform/tofu apply".into() },
            CompletionItem { insert: "validate".into(), hint: "Validate configuration files".into() },
            CompletionItem { insert: "drift".into(), hint: "Detect infrastructure drift".into() },
        ],
        "/pipeline" => vec![
            CompletionItem { insert: "generate".into(), hint: "Generate CI config from project type".into() },
            CompletionItem { insert: "lint".into(), hint: "Validate existing CI pipeline config".into() },
            CompletionItem { insert: "run".into(), hint: "Trigger local pipeline run via act".into() },
        ],
        "/review" => vec![
            CompletionItem { insert: "staged".into(), hint: "Review all staged git changes".into() },
            CompletionItem { insert: "pr".into(), hint: "Review current PR diff".into() },
        ],
        "/deps" => vec![
            CompletionItem { insert: "tree".into(), hint: "Show dependency tree".into() },
            CompletionItem { insert: "outdated".into(), hint: "Show outdated dependencies".into() },
            CompletionItem { insert: "audit".into(), hint: "Security audit of dependencies".into() },
            CompletionItem { insert: "why".into(), hint: "Explain why a dependency is included".into() },
        ],
        "/mono" => vec![
            CompletionItem { insert: "list".into(), hint: "List workspace packages".into() },
            CompletionItem { insert: "graph".into(), hint: "Show package dependency graph".into() },
            CompletionItem { insert: "changed".into(), hint: "Packages changed since last release".into() },
            CompletionItem { insert: "run".into(), hint: "Run command in workspace packages".into() },
        ],
        "/browser" => vec![
            CompletionItem { insert: "open".into(), hint: "Open URL in default browser".into() },
            CompletionItem { insert: "screenshot".into(), hint: "Capture screenshot (playwright)".into() },
            CompletionItem { insert: "test".into(), hint: "Run accessibility/performance test".into() },
        ],
        "/notify" => vec![
            CompletionItem { insert: "send".into(), hint: "Send a desktop notification".into() },
            CompletionItem { insert: "webhook".into(), hint: "POST message to a webhook URL".into() },
            CompletionItem { insert: "matrix".into(), hint: "Send to Matrix room (MATRIX_TOKEN)".into() },
            CompletionItem { insert: "discord".into(), hint: "Send to Discord channel via webhook".into() },
            CompletionItem { insert: "slack".into(), hint: "Send to Slack channel via webhook".into() },
            CompletionItem { insert: "telegram".into(), hint: "Send to Telegram chat (TELEGRAM_BOT_TOKEN)".into() },
            CompletionItem { insert: "whatsapp".into(), hint: "Send via WhatsApp Business API".into() },
            CompletionItem { insert: "signal".into(), hint: "Send via Signal (signal-cli)".into() },
        ],
        "/migrate" => vec![
            CompletionItem { insert: "framework".into(), hint: "Migrate between frameworks (e.g. react → vue)".into() },
            CompletionItem { insert: "language".into(), hint: "Convert codebase to another language".into() },
            CompletionItem { insert: "deps".into(), hint: "Migrate package manager (npm/yarn/pnpm)".into() },
        ],
        "/regex" => vec![
            CompletionItem { insert: "build".into(), hint: "Generate regex from a natural language description".into() },
            CompletionItem { insert: "test".into(), hint: "Test a pattern against an input string".into() },
            CompletionItem { insert: "explain".into(), hint: "Explain a regex pattern in plain English".into() },
        ],
        "/ssh" => vec![
            CompletionItem { insert: "list".into(), hint: "List hosts from ~/.ssh/config".into() },
            CompletionItem { insert: "connect".into(), hint: "Show command to connect to a host".into() },
            CompletionItem { insert: "tunnel".into(), hint: "Set up an SSH port tunnel".into() },
            CompletionItem { insert: "keys".into(), hint: "List SSH keys in ~/.ssh/".into() },
        ],
        "/logs" => vec![
            CompletionItem { insert: "tail".into(), hint: "Show last 50 lines of a log file".into() },
            CompletionItem { insert: "search".into(), hint: "Search log file with context lines".into() },
            CompletionItem { insert: "analyze".into(), hint: "AI-powered log analysis for errors/patterns".into() },
            CompletionItem { insert: "stats".into(), hint: "Show error/warn/info counts".into() },
        ],
        "/markdown" | "/md" => vec![
            CompletionItem { insert: "preview".into(), hint: "Render markdown in TUI (stripped)".into() },
            CompletionItem { insert: "toc".into(), hint: "Generate table of contents from headings".into() },
            CompletionItem { insert: "lint".into(), hint: "Check for trailing whitespace and long lines".into() },
        ],
        "/snippets" => vec![
            CompletionItem { insert: "save".into(), hint: "Save code as a named snippet".into() },
            CompletionItem { insert: "list".into(), hint: "List all saved snippets".into() },
            CompletionItem { insert: "get".into(), hint: "Retrieve a snippet by name".into() },
            CompletionItem { insert: "search".into(), hint: "Search snippets by name or content".into() },
        ],
        "/finetune" => vec![
            CompletionItem { insert: "prepare".into(), hint: "Review training data file quality".into() },
            CompletionItem { insert: "validate".into(), hint: "Validate JSONL format line by line".into() },
            CompletionItem { insert: "start".into(), hint: "Show steps to submit fine-tuning job".into() },
            CompletionItem { insert: "status".into(), hint: "Check fine-tuning job status via CLI".into() },
        ],
        "/webhook" => vec![
            CompletionItem { insert: "list".into(), hint: "List configured webhook endpoints".into() },
            CompletionItem { insert: "add".into(), hint: "Add a new webhook endpoint".into() },
            CompletionItem { insert: "test".into(), hint: "Send test payload to a webhook".into() },
            CompletionItem { insert: "remove".into(), hint: "Remove a webhook endpoint".into() },
        ],
        "/plugin-sdk" => vec![
            CompletionItem { insert: "init".into(), hint: "Scaffold a new plugin project".into() },
            CompletionItem { insert: "build".into(), hint: "Build and type-check the plugin".into() },
            CompletionItem { insert: "test".into(), hint: "Run plugin test suite".into() },
            CompletionItem { insert: "publish".into(), hint: "Publish plugin to AnvilHub".into() },
        ],
        _ => vec![],
    }
}

/// Third-level completions — shown after "/command subcommand ".
pub(super) fn third_level_completions(command: &str, subcommand: &str) -> Vec<CompletionItem> {
    match (command, subcommand.trim()) {
        ("/theme", "set") => vec![
            CompletionItem { insert: "cyberpunk".into(), hint: "Neon pink + electric blue".into() },
            CompletionItem { insert: "nord".into(), hint: "Arctic blues + muted greens".into() },
            CompletionItem { insert: "solarized-dark".into(), hint: "Classic calibrated colors".into() },
            CompletionItem { insert: "dracula".into(), hint: "Purple + pink + green".into() },
            CompletionItem { insert: "culpur-defense".into(), hint: "Navy + cyan (default)".into() },
            CompletionItem { insert: "monokai".into(), hint: "Gold + green + magenta classic".into() },
            CompletionItem { insert: "gruvbox".into(), hint: "Warm retro browns + gold".into() },
            CompletionItem { insert: "catppuccin".into(), hint: "Pastel lavender + pink".into() },
        ],
        ("/scaffold", "new") => vec![
            CompletionItem { insert: "rust".into(), hint: "Rust binary project (Cargo)".into() },
            CompletionItem { insert: "node".into(), hint: "Node.js project (npm)".into() },
            CompletionItem { insert: "python".into(), hint: "Python project (venv + pyproject.toml)".into() },
            CompletionItem { insert: "react".into(), hint: "React + TypeScript (Vite)".into() },
            CompletionItem { insert: "nextjs".into(), hint: "Next.js + TypeScript".into() },
            CompletionItem { insert: "go".into(), hint: "Go module project".into() },
            CompletionItem { insert: "docker".into(), hint: "Dockerfile + compose boilerplate".into() },
        ],
        ("/provider", "anthropic" | "openai" | "ollama") => vec![
            CompletionItem { insert: "login".into(), hint: "Login/refresh credentials".into() },
        ],
        ("/configure", "providers") => vec![
            CompletionItem { insert: "anthropic".into(), hint: "Anthropic settings".into() },
            CompletionItem { insert: "openai".into(), hint: "OpenAI settings".into() },
            CompletionItem { insert: "ollama".into(), hint: "Ollama settings".into() },
        ],
        ("/configure", "models") => vec![
            CompletionItem { insert: "default".into(), hint: "Set default model".into() },
            CompletionItem { insert: "image".into(), hint: "Set image model".into() },
        ],
        ("/configure", "context") => vec![
            CompletionItem { insert: "size".into(), hint: "Set context size (e.g. 1M)".into() },
            CompletionItem { insert: "threshold".into(), hint: "Set auto-compact % (e.g. 85)".into() },
            CompletionItem { insert: "qmd".into(), hint: "Toggle QMD (on/off)".into() },
        ],
        ("/configure", "display") => vec![
            CompletionItem { insert: "vim".into(), hint: "Toggle vim mode (on/off)".into() },
            CompletionItem { insert: "chat".into(), hint: "Toggle chat mode (on/off)".into() },
        ],
        ("/failover", "add" | "remove") => vec![
            CompletionItem { insert: "claude-opus-4-6".into(), hint: "Anthropic Opus".into() },
            CompletionItem { insert: "claude-sonnet-4-6".into(), hint: "Anthropic Sonnet".into() },
            CompletionItem { insert: "gpt-5.4-mini".into(), hint: "OpenAI GPT-5.4 Mini".into() },
            CompletionItem { insert: "llama3.2".into(), hint: "Ollama local".into() },
        ],
        ("/configure", "size") => vec![
            CompletionItem { insert: "200K".into(), hint: "200,000 tokens".into() },
            CompletionItem { insert: "500K".into(), hint: "500,000 tokens".into() },
            CompletionItem { insert: "1M".into(), hint: "1,000,000 tokens (default)".into() },
            CompletionItem { insert: "2M".into(), hint: "2,000,000 tokens".into() },
        ],
        ("/configure", "threshold") => vec![
            CompletionItem { insert: "75".into(), hint: "75% of context window".into() },
            CompletionItem { insert: "80".into(), hint: "80% of context window".into() },
            CompletionItem { insert: "85".into(), hint: "85% (default)".into() },
            CompletionItem { insert: "90".into(), hint: "90% of context window".into() },
            CompletionItem { insert: "95".into(), hint: "95% of context window".into() },
        ],
        ("/configure", "qmd") => vec![
            CompletionItem { insert: "on".into(), hint: "Enable QMD integration".into() },
            CompletionItem { insert: "off".into(), hint: "Disable QMD integration".into() },
        ],
        ("/configure", "vim" | "chat") => vec![
            CompletionItem { insert: "on".into(), hint: "Enable".into() },
            CompletionItem { insert: "off".into(), hint: "Disable".into() },
        ],
        ("/configure", "default" | "image") => vec![
            CompletionItem { insert: "claude-opus-4-6".into(), hint: "Anthropic Opus".into() },
            CompletionItem { insert: "claude-sonnet-4-6".into(), hint: "Anthropic Sonnet".into() },
            CompletionItem { insert: "gpt-5.4-mini".into(), hint: "OpenAI GPT-5.4 Mini".into() },
            CompletionItem { insert: "gpt-image-1.5".into(), hint: "OpenAI Image Gen".into() },
            CompletionItem { insert: "llama3.2".into(), hint: "Ollama local".into() },
        ],
        ("/configure", "tavily" | "brave" | "exa" | "perplexity" | "bing" | "google") => vec![
            CompletionItem { insert: "<api-key>".into(), hint: "Paste your API key".into() },
        ],
        ("/configure", "wp") => vec![
            CompletionItem { insert: "<url>".into(), hint: "WordPress URL (e.g. https://culpur.net)".into() },
        ],
        ("/configure", "github") => vec![
            CompletionItem { insert: "<token>".into(), hint: "GitHub personal access token".into() },
        ],
        ("/login", _) if !subcommand.trim().is_empty() => vec![],
        ("/vault", "totp") => vec![
            CompletionItem { insert: "add".into(), hint: "Add a TOTP entry (Base32 secret)".into() },
            CompletionItem { insert: "list".into(), hint: "List TOTP labels".into() },
            CompletionItem { insert: "delete".into(), hint: "Delete a TOTP entry".into() },
        ],
        ("/lsp", "start") => vec![
            CompletionItem { insert: "rust".into(), hint: "rust-analyzer".into() },
            CompletionItem { insert: "typescript".into(), hint: "typescript-language-server".into() },
            CompletionItem { insert: "python".into(), hint: "pylsp".into() },
            CompletionItem { insert: "go".into(), hint: "gopls".into() },
            CompletionItem { insert: "java".into(), hint: "jdtls".into() },
        ],
        ("/notebook", "export") => vec![
            CompletionItem { insert: "html".into(), hint: "Export as HTML".into() },
            CompletionItem { insert: "py".into(), hint: "Export as Python script".into() },
            CompletionItem { insert: "pdf".into(), hint: "Export as PDF (requires LaTeX)".into() },
        ],
        ("/mono", "run") => vec![
            CompletionItem { insert: "build".into(), hint: "Build all packages".into() },
            CompletionItem { insert: "test".into(), hint: "Test all packages".into() },
            CompletionItem { insert: "lint".into(), hint: "Lint all packages".into() },
            CompletionItem { insert: "clean".into(), hint: "Clean build artifacts".into() },
        ],
        _ => vec![],
    }
}

/// Update the completion popup based on current input.
pub(super) fn update_completions(input: &str) -> CompletionPopup {
    if input.is_empty() || !input.starts_with('/') {
        return CompletionPopup::default();
    }

    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let command = parts[0];

    if parts.len() == 1 && !input.ends_with(' ') {
        let matches: Vec<CompletionItem> = all_slash_commands()
            .into_iter()
            .filter(|c| c.insert.starts_with(input))
            .collect();

        if matches.is_empty() {
            return CompletionPopup::default();
        }

        if matches.len() == 1 && matches[0].insert == input {
            let subs = subcommands_for(input);
            if !subs.is_empty() {
                return CompletionPopup {
                    visible: true,
                    matches: subs,
                    selected: 0,
                };
            }
            return CompletionPopup::default();
        }

        CompletionPopup {
            visible: true,
            matches,
            selected: 0,
        }
    } else {
        let remainder = parts.get(1).unwrap_or(&"").to_string();
        let words: Vec<&str> = remainder.split_whitespace().collect();

        if words.is_empty() {
            let subs = subcommands_for(command);
            if subs.is_empty() {
                return CompletionPopup::default();
            }
            return CompletionPopup { visible: true, matches: subs, selected: 0 };
        }

        if words.len() == 1 && !remainder.ends_with(' ') {
            let prefix = words[0];
            let subs = subcommands_for(command);
            let matches: Vec<CompletionItem> = subs.into_iter()
                .filter(|c| c.insert.starts_with(prefix))
                .collect();
            if matches.is_empty() || (matches.len() == 1 && matches[0].insert == prefix) {
                if matches.len() == 1 && matches[0].insert == prefix {
                    let third = third_level_completions(command, prefix);
                    if !third.is_empty() {
                        return CompletionPopup { visible: true, matches: third, selected: 0 };
                    }
                }
                return CompletionPopup::default();
            }
            return CompletionPopup { visible: true, matches, selected: 0 };
        }

        if words.len() == 1 && remainder.ends_with(' ') {
            let subcmd = words[0];
            let third = third_level_completions(command, subcmd);
            if !third.is_empty() {
                return CompletionPopup { visible: true, matches: third, selected: 0 };
            }
            return CompletionPopup::default();
        }

        if words.len() == 2 && !remainder.ends_with(' ') {
            let subcmd = words[0];
            let prefix = words[1];
            let third = third_level_completions(command, subcmd);
            let matches: Vec<CompletionItem> = third.into_iter()
                .filter(|c| c.insert.starts_with(prefix))
                .collect();
            if matches.len() == 1 && matches[0].insert == prefix {
                let fourth = third_level_completions(command, prefix);
                if !fourth.is_empty() {
                    return CompletionPopup { visible: true, matches: fourth, selected: 0 };
                }
                return CompletionPopup::default();
            }
            if matches.is_empty() {
                return CompletionPopup::default();
            }
            return CompletionPopup { visible: true, matches, selected: 0 };
        }

        if words.len() == 2 && remainder.ends_with(' ') {
            let value = words[1];
            let fourth = third_level_completions(command, value);
            if !fourth.is_empty() {
                return CompletionPopup { visible: true, matches: fourth, selected: 0 };
            }
            return CompletionPopup::default();
        }

        if words.len() >= 3 {
            let value = words[1];
            let prefix = words[2];
            let fourth = third_level_completions(command, value);
            let matches: Vec<CompletionItem> = fourth.into_iter()
                .filter(|c| c.insert.starts_with(prefix))
                .collect();
            if matches.is_empty() {
                return CompletionPopup::default();
            }
            return CompletionPopup { visible: true, matches, selected: 0 };
        }

        CompletionPopup::default()
    }
}

// ─── Ollama model cache ───────────────────────────────────────────────────────

static OLLAMA_MODEL_CACHE: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();

pub fn init_ollama_model_cache() {
    let _ = OLLAMA_MODEL_CACHE.get_or_init(|| {
        let ollama_url = std::env::var("OLLAMA_HOST")
            .unwrap_or_else(|_| "http://localhost:11434".to_string());
        let output = std::process::Command::new("curl")
            .args(["-s", "--max-time", "2", &format!("{ollama_url}/api/tags")])
            .output();
        match output {
            Ok(o) if o.status.success() => {
                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&o.stdout) {
                    if let Some(arr) = val.get("models").and_then(|m| m.as_array()) {
                        return arr.iter().filter_map(|m| {
                            let name = m.get("name").and_then(|n| n.as_str())?;
                            let size = m.get("size").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                            let gb = size / 1_000_000_000.0;
                            Some((name.to_string(), format!("{gb:.1}GB")))
                        }).collect();
                    }
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    });
}

pub(super) fn cached_ollama_models() -> Vec<(String, String)> {
    OLLAMA_MODEL_CACHE.get().cloned().unwrap_or_default()
}

// ─── Clipboard image paste ────────────────────────────────────────────────────

/// Attempt to read a PNG image from the system clipboard.
pub fn check_clipboard_for_image() -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        let script = r#"
try
    set imgData to the clipboard as «class PNGf»
    set hexStr to ""
    repeat with b in imgData
        set hexStr to hexStr & (do shell script "printf '%02x' " & (b as integer))
    end repeat
    return hexStr
on error
    return ""
end try
"#;
        let output = std::process::Command::new("osascript")
            .args(["-e", script])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let hex = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if hex.is_empty() {
            return None;
        }
        let bytes: Option<Vec<u8>> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
            .collect();
        bytes.filter(|b| !b.is_empty())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let output = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "image/png", "-o"])
            .output()
            .ok()?;
        if output.status.success() && !output.stdout.is_empty() {
            return Some(output.stdout);
        }
        None
    }
}
