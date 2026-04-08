#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommandCategory {
    Core,
    Workspace,
    Session,
    Git,
    Automation,
}

impl SlashCommandCategory {
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Core => "Core flow",
            Self::Workspace => "Workspace & memory",
            Self::Session => "Sessions & output",
            Self::Git => "Git & GitHub",
            Self::Automation => "Automation & discovery",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashCommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub summary: &'static str,
    pub argument_hint: Option<&'static str>,
    pub resume_supported: bool,
    pub category: SlashCommandCategory,
    /// Multi-line detailed help shown when the user types `/help <command>`.
    /// An empty string means no detailed help is available for this command.
    pub detailed_help: &'static str,
}

const SLASH_COMMAND_SPECS: &[SlashCommandSpec] = &[
    SlashCommandSpec {
        name: "help",
        aliases: &[],
        summary: "Show available slash commands",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "status",
        aliases: &[],
        summary: "Show current session status",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "compact",
        aliases: &[],
        summary: "Compact local session history",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "model",
        aliases: &[],
        summary: "Show or switch the active model",
        argument_hint: Some("[model]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "\
/model — Show or switch the active AI model

Usage:
  /model             Show the currently active model and conversation stats
  /model <name>      Switch to a different model (takes effect immediately)

Supported aliases:
  opus, sonnet, haiku          → Anthropic Claude latest variants
  gpt4, gpt4o, gpt4o-mini      → OpenAI GPT-4 variants
  o1, o1-mini, o3-mini         → OpenAI reasoning models

Examples:
  /model
  /model opus
  /model claude-sonnet-4-5",
    },
    SlashCommandSpec {
        name: "permissions",
        aliases: &[],
        summary: "Show or switch the active permission mode",
        argument_hint: Some("[read-only|workspace-write|danger-full-access]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "clear",
        aliases: &[],
        summary: "Start a fresh local session",
        argument_hint: Some("[--confirm]"),
        resume_supported: true,
        category: SlashCommandCategory::Session,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "cost",
        aliases: &[],
        summary: "Show cumulative token usage for this session",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "provider",
        aliases: &["providers"],
        summary: "Show, switch, or list providers and their models",
        argument_hint: Some("[anthropic|openai|ollama|list|login]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "\
/provider (alias: /providers) — Manage AI providers

Usage:
  /provider              Show the active provider and model
  /provider list         List all configured providers and their available models
  /provider anthropic    Switch to the Anthropic (Claude) provider
  /provider openai       Switch to the OpenAI provider
  /provider ollama       Switch to a local Ollama instance
  /provider login        Authenticate the current provider

Examples:
  /provider
  /provider list
  /provider openai
  /provider login",
    },
    SlashCommandSpec {
        name: "login",
        aliases: &[],
        summary: "Login or refresh credentials for a provider",
        argument_hint: Some("[anthropic|openai|ollama]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "resume",
        aliases: &[],
        summary: "Load a saved session into the REPL",
        argument_hint: Some("<session-path>"),
        resume_supported: false,
        category: SlashCommandCategory::Session,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "config",
        aliases: &[],
        summary: "Inspect Anvil config files or merged sections",
        argument_hint: Some("[env|hooks|model|plugins]"),
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "memory",
        aliases: &[],
        summary: "Inspect loaded Anvil instruction memory files",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "init",
        aliases: &[],
        summary: "Create a starter ANVIL.md for this repo",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "diff",
        aliases: &[],
        summary: "Show git diff for current workspace changes",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "version",
        aliases: &[],
        summary: "Show CLI version and build information",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "bughunter",
        aliases: &[],
        summary: "Inspect the codebase for likely bugs",
        argument_hint: Some("[scope]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "branch",
        aliases: &[],
        summary: "List, create, or switch git branches",
        argument_hint: Some("[list|create <name>|switch <name>]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "worktree",
        aliases: &[],
        summary: "List, add, remove, or prune git worktrees",
        argument_hint: Some("[list|add <path> [branch]|remove <path>|prune]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "commit",
        aliases: &[],
        summary: "Generate a commit message and create a git commit",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "commit-push-pr",
        aliases: &[],
        summary: "Commit workspace changes, push the branch, and open a PR",
        argument_hint: Some("[context]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "pr",
        aliases: &[],
        summary: "Draft or create a pull request from the conversation",
        argument_hint: Some("[context]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "issue",
        aliases: &[],
        summary: "Draft or create a GitHub issue from the conversation",
        argument_hint: Some("[context]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "ultraplan",
        aliases: &[],
        summary: "Run a deep planning prompt with multi-step reasoning",
        argument_hint: Some("[task]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "teleport",
        aliases: &[],
        summary: "Jump to a file or symbol by searching the workspace",
        argument_hint: Some("<symbol-or-path>"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "debug-tool-call",
        aliases: &[],
        summary: "Replay the last tool call with debug details",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "export",
        aliases: &[],
        summary: "Export the current conversation to a file",
        argument_hint: Some("[file]"),
        resume_supported: true,
        category: SlashCommandCategory::Session,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "session",
        aliases: &[],
        summary: "List or switch managed local sessions",
        argument_hint: Some("[list|switch <session-id>]"),
        resume_supported: false,
        category: SlashCommandCategory::Session,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "plugin",
        aliases: &["plugins", "marketplace"],
        summary: "Manage Anvil plugins",
        argument_hint: Some(
            "[list|install <path>|enable <name>|disable <name>|uninstall <id>|update <id>]",
        ),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "agents",
        aliases: &[],
        summary: "List configured agents",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "skills",
        aliases: &[],
        summary: "List available skills",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "qmd",
        aliases: &[],
        summary: "Search the local markdown knowledge base via QMD",
        argument_hint: Some("<query>"),
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "undo",
        aliases: &[],
        summary: "Undo last file change (unstaged diff or last commit)",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "history",
        aliases: &[],
        summary: "Show conversation history (last 20 messages)",
        argument_hint: Some("[all]"),
        resume_supported: true,
        category: SlashCommandCategory::Session,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "context",
        aliases: &[],
        summary: "Add a file to context, or list context files",
        argument_hint: Some("[path]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "pin",
        aliases: &[],
        summary: "Pin a file to always-in-context (persists across sessions)",
        argument_hint: Some("[path]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "unpin",
        aliases: &[],
        summary: "Remove a pinned file",
        argument_hint: Some("<path>"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "chat",
        aliases: &[],
        summary: "Toggle chat-only mode (disables all tools)",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "vim",
        aliases: &[],
        summary: "Toggle vim keybindings in the input editor",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "web",
        aliases: &[],
        summary: "Quick web search — results shown inline without a model turn",
        argument_hint: Some("<query>"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "doctor",
        aliases: &[],
        summary: "Diagnose Anvil configuration and dependencies",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "tokens",
        aliases: &[],
        summary: "Detailed per-turn and cumulative token breakdown with cost estimate",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "search",
        aliases: &[],
        summary: "Multi-provider web search (duckduckgo, tavily, brave, exa, …)",
        argument_hint: Some("[provider <name>] <query> | providers | config <key> <val>"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "failover",
        aliases: &[],
        summary: "Manage AI provider failover chain (rate-limit handling)",
        argument_hint: Some("[status | add <model> | remove <model> | reset]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "generate-image",
        aliases: &["image"],
        summary: "Generate an image with OpenAI and download it locally",
        argument_hint: Some("[--wp <post-id>] <prompt>"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "history-archive",
        aliases: &[],
        summary: "Browse, search, or view archived session history",
        argument_hint: Some("[search <query> | view <session-id>]"),
        resume_supported: true,
        category: SlashCommandCategory::Session,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "configure",
        aliases: &["settings", "config-menu"],
        summary: "Interactive configuration wizard — providers, models, context, search, …",
        argument_hint: Some("[providers|models|context|search|permissions|display|integrations]"),
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    SlashCommandSpec {
        name: "theme",
        aliases: &[],
        summary: "Show, list, or change the TUI colour theme; create/import/export custom themes",
        argument_hint: Some("[list | set <name> | reset | create <name> | import <file> | export <name>]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    // Feature 3 — semantic code search
    SlashCommandSpec {
        name: "semantic-search",
        aliases: &["symsearch"],
        summary: "Search for symbols (functions, classes, structs, imports) grouped by type",
        argument_hint: Some("<query> [--lang <ext>] [--type fn|class|struct|import]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    // Feature 4 — Docker / container awareness
    SlashCommandSpec {
        name: "docker",
        aliases: &[],
        summary: "Inspect and manage Docker containers and compose services",
        argument_hint: Some("[ps|logs <container>|compose|build]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "\
/docker — Docker container and compose service inspector

Subcommands:
  /docker ps               List running containers (equivalent to docker ps)
  /docker logs <name>      Tail the last 50 lines of a container's logs
  /docker compose          Show docker-compose.yml services and their status
  /docker build            Build images defined in docker-compose.yml

Examples:
  /docker ps
  /docker logs api-server
  /docker compose
  /docker build",
    },
    // Feature 5 — test generation
    SlashCommandSpec {
        name: "test",
        aliases: &[],
        summary: "Generate unit tests, run the test suite, or show coverage",
        argument_hint: Some("[generate <file>|run|coverage]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "\
/test — Test generation and execution helpers

Subcommands:
  /test generate <file>   AI-generate unit tests for a source file
  /test run               Run the project test suite (auto-detects cargo/npm/pytest/…)
  /test coverage          Show test coverage report

Examples:
  /test generate src/api/routes.rs
  /test run
  /test coverage",
    },
    // Feature 6 — advanced git
    SlashCommandSpec {
        name: "git",
        aliases: &[],
        summary: "Advanced git helpers: rebase, conflicts, cherry-pick, stash",
        argument_hint: Some("[rebase|conflicts|cherry-pick <sha>|stash [list|pop|drop]]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",
    },
    // Feature 7 — refactoring tools
    SlashCommandSpec {
        name: "refactor",
        aliases: &[],
        summary: "Refactoring helpers: rename, extract function, move code",
        argument_hint: Some("[rename <old> <new>|extract <file> <lines>|move <src> <dst>]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature 8 — screenshot / clipboard image input
    SlashCommandSpec {
        name: "screenshot",
        aliases: &[],
        summary: "Capture a screenshot and send it to the AI for analysis",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    // Feature 9 — database tools
    SlashCommandSpec {
        name: "db",
        aliases: &[],
        summary: "Database tools — connect, inspect schema, run queries, suggest migrations",
        argument_hint: Some("[connect <url>|schema|query <sql>|migrate]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature 10 — security scanning
    SlashCommandSpec {
        name: "security",
        aliases: &[],
        summary: "Security scanning — vulnerabilities, secrets, dependency CVEs, report",
        argument_hint: Some("[scan|secrets|deps|report]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "\
/security — AI-assisted security scanning

Subcommands:
  /security scan        Static analysis scan for common vulnerabilities (XSS, SQLi, path traversal, …)
  /security secrets     Search for accidentally committed secrets and API keys
  /security deps        Audit dependencies for known CVEs
  /security report      Generate a full security report combining all scans

Examples:
  /security scan
  /security secrets
  /security deps
  /security report",
    },
    // Feature 11 — API development helpers
    SlashCommandSpec {
        name: "api",
        aliases: &[],
        summary: "API development helpers — spec, mock server, endpoint test, docs",
        argument_hint: Some("[spec <file>|mock <spec>|test <endpoint>|docs]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature 12 — documentation generation
    SlashCommandSpec {
        name: "docs",
        aliases: &[],
        summary: "Documentation generation — readme, architecture, changelog",
        argument_hint: Some("[generate|readme|architecture|changelog]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature 13 — project scaffolding
    SlashCommandSpec {
        name: "scaffold",
        aliases: &[],
        summary: "Scaffold a new project from a template (rust, node, python, react, nextjs, go, docker)",
        argument_hint: Some("[new <template>|list]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    // Feature 14 — performance profiling
    SlashCommandSpec {
        name: "perf",
        aliases: &[],
        summary: "Performance profiling and benchmarking helpers",
        argument_hint: Some("[profile <command>|benchmark <file>|flamegraph|analyze]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature 15 — debugging integration
    SlashCommandSpec {
        name: "debug",
        aliases: &[],
        summary: "Debugging helpers — start debugger, set breakpoints, watch expressions, explain errors",
        argument_hint: Some("[start <file>|breakpoint <file:line>|watch <expr>|explain <error>]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature 16 — voice input (placeholder)
    SlashCommandSpec {
        name: "voice",
        aliases: &[],
        summary: "Voice input — start/stop microphone capture (coming soon)",
        argument_hint: Some("[start|stop]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    // Feature 17 — collaboration (placeholder)
    SlashCommandSpec {
        name: "collab",
        aliases: &[],
        summary: "Collaborative session sharing — share or join a session (coming soon)",
        argument_hint: Some("[share|join <id>]"),
        resume_supported: false,
        category: SlashCommandCategory::Session,
        detailed_help: "",
    },
    // Feature 19 — changelog generator
    SlashCommandSpec {
        name: "changelog",
        aliases: &[],
        summary: "Generate a CHANGELOG.md entry from git log since the last tag",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",
    },
    // Feature 20 — environment manager
    SlashCommandSpec {
        name: "env",
        aliases: &[],
        summary: "Manage session environment variables — show, set, load .env, diff environments",
        argument_hint: Some("[show|set <key> <value>|load <file>|diff]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    // AnvilHub marketplace
    SlashCommandSpec {
        name: "hub",
        aliases: &[],
        summary: "Browse and install packages from the AnvilHub marketplace",
        argument_hint: Some("[search <q>|skills|plugins|agents|themes|install <name>|info <name>]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "\
/hub — AnvilHub marketplace for skills, plugins, agents and themes

Subcommands:
  /hub search <query>     Search the marketplace
  /hub skills             Browse available skills
  /hub plugins            Browse available plugins
  /hub agents             Browse available agent definitions
  /hub themes             Browse available TUI themes
  /hub install <name>     Install a package by name
  /hub info <name>        Show details and README for a package

Examples:
  /hub search rust-review
  /hub skills
  /hub install devops-expert
  /hub info security-scanner",
    },
    // i18n language switcher
    SlashCommandSpec {
        name: "language",
        aliases: &["lang"],
        summary: "Set display language (en, de, es, fr, ja, zh-CN, ru)",
        argument_hint: Some("[en|de|es|fr|ja|zh-CN|ru]"),
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    // Feature A — LSP autocomplete
    SlashCommandSpec {
        name: "lsp",
        aliases: &[],
        summary: "Language server protocol helpers — start, list symbols, find references",
        argument_hint: Some("[start <lang>|symbols <file>|references <symbol>]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    // Feature B — Jupyter notebook support
    SlashCommandSpec {
        name: "notebook",
        aliases: &[],
        summary: "Jupyter notebook runner — execute, run a cell, or export",
        argument_hint: Some("[run <file>|cell <file> <n>|export <file> <format>]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature C — Kubernetes management
    SlashCommandSpec {
        name: "k8s",
        aliases: &["kubectl"],
        summary: "Kubernetes helpers — pods, logs, apply, describe",
        argument_hint: Some("[pods|logs <pod>|apply <file>|describe <resource>]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "\
/k8s (alias: /kubectl) — Kubernetes cluster helpers

Subcommands:
  /k8s pods                   List all pods in the current namespace
  /k8s logs <pod>             Stream recent logs from a pod
  /k8s apply <file>           Apply a manifest file (kubectl apply -f)
  /k8s describe <resource>    Describe a resource (pod, svc, deployment, …)

Examples:
  /k8s pods
  /k8s logs api-pod-7d9f8b-xyz
  /k8s apply k8s/deployment.yaml
  /k8s describe deployment/api",
    },
    // Feature D — Terraform/IaC
    SlashCommandSpec {
        name: "iac",
        aliases: &["terraform"],
        summary: "Infrastructure-as-code helpers — plan, apply, validate, drift",
        argument_hint: Some("[plan|apply|validate|drift]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature E — CI/CD pipeline builder
    SlashCommandSpec {
        name: "pipeline",
        aliases: &[],
        summary: "CI/CD pipeline builder — generate, lint, or run a local pipeline",
        argument_hint: Some("[generate|lint|run]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature F — Code review
    SlashCommandSpec {
        name: "review",
        aliases: &[],
        summary: "AI-powered code review — file, staged changes, or PR diff",
        argument_hint: Some("[<file>|staged|pr]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "\
/review — AI-powered code review

Subcommands:
  /review <file>     Review a specific source file
  /review staged     Review all currently staged git changes
  /review pr         Review the diff between HEAD and origin/main

The review covers: bugs, security vulnerabilities, performance issues,
style concerns, and suggested improvements.

Examples:
  /review src/main.rs
  /review staged
  /review pr",
    },
    // Feature G — Dependency graph
    SlashCommandSpec {
        name: "deps",
        aliases: &[],
        summary: "Dependency graph helpers — tree, outdated, audit, why",
        argument_hint: Some("[tree|outdated|audit|why <pkg>]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    // Feature H — Monorepo awareness
    SlashCommandSpec {
        name: "mono",
        aliases: &[],
        summary: "Monorepo workspace helpers — list, graph, changed, run",
        argument_hint: Some("[list|graph|changed|run <cmd> [--filter <pkg>]]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    // Feature I — Browser automation
    SlashCommandSpec {
        name: "browser",
        aliases: &[],
        summary: "Browser automation — open URL, screenshot, accessibility test",
        argument_hint: Some("[open <url>|screenshot <url>|test <url>]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature J — Desktop & webhook notifications
    SlashCommandSpec {
        name: "notify",
        aliases: &[],
        summary: "Send notifications — desktop, webhook, or Matrix room",
        argument_hint: Some("[send <msg>|webhook <url> <msg>|matrix <room> <msg>]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "\
/notify — Send notifications from Anvil

Subcommands:
  /notify send <msg>                 Send a desktop notification
  /notify webhook <url> <msg>        POST a message to a webhook URL
  /notify matrix <room-id> <msg>     Send a message to a Matrix room

Examples:
  /notify send \"Build complete\"
  /notify webhook https://hooks.example.com/abc \"Deploy done\"
  /notify matrix !room:server.org \"Agent finished\"",
    },
    // Feature 21 — Credential Vault
    SlashCommandSpec {
        name: "vault",
        aliases: &[],
        summary: "Encrypted credential vault and TOTP manager (AES-256-GCM + Argon2id)",
        argument_hint: Some("[setup|unlock|lock|store <label>|get <label>|list|delete <label>|totp add <label>|totp <label>|totp list|totp delete <label>]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "\
/vault — Encrypted credential vault (AES-256-GCM + Argon2id key derivation)

Subcommands:
  /vault setup                  Initialise a new vault (prompts for master password)
  /vault unlock                 Unlock the vault for this session
  /vault lock                   Re-lock the vault immediately
  /vault store <label>          Securely store a new credential under <label>
  /vault get <label>            Retrieve a stored credential by label
  /vault list                   List all stored credential labels
  /vault delete <label>         Delete a credential permanently

TOTP (one-time passwords):
  /vault totp add <label>       Add a TOTP secret (prompts for base32 seed)
  /vault totp <label>           Generate the current 6-digit TOTP code
  /vault totp list              List all TOTP labels
  /vault totp delete <label>    Remove a TOTP entry

Examples:
  /vault setup
  /vault store github-token
  /vault get github-token
  /vault totp add aws-mfa",
    },
    // Feature 11 — codebase migration
    SlashCommandSpec {
        name: "migrate",
        aliases: &[],
        summary: "AI-assisted codebase migration — framework, language, or dependency manager",
        argument_hint: Some("[framework <from> <to>|language <from> <to>|deps]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature 12 — regex builder / tester
    SlashCommandSpec {
        name: "regex",
        aliases: &[],
        summary: "Regex builder and tester — build from description, test, or explain a pattern",
        argument_hint: Some("[build <description>|test <pattern> <input>|explain <pattern>]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature 13 — SSH session manager
    SlashCommandSpec {
        name: "ssh",
        aliases: &[],
        summary: "SSH session manager — list hosts, connect, set up tunnel, or list keys",
        argument_hint: Some("[list|connect <host>|tunnel <host> <local:remote>|keys]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature 14 — log analysis
    SlashCommandSpec {
        name: "logs",
        aliases: &[],
        summary: "Log analysis — tail, search, AI-powered analysis, or statistics",
        argument_hint: Some("[tail <file>|search <file> <pattern>|analyze <file>|stats <file>]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature 15 — markdown preview
    SlashCommandSpec {
        name: "markdown",
        aliases: &["md"],
        summary: "Markdown helpers — preview in TUI, generate table of contents, or lint",
        argument_hint: Some("[preview <file>|toc <file>|lint <file>]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    // Feature 16 — snippet library
    SlashCommandSpec {
        name: "snippets",
        aliases: &[],
        summary: "Snippet library — save, list, retrieve, or search code snippets",
        argument_hint: Some("[save <name>|list|get <name>|search <query>]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
    },
    // Feature 17 — AI fine-tuning assistant
    SlashCommandSpec {
        name: "finetune",
        aliases: &[],
        summary: "AI fine-tuning assistant — prepare data, validate, submit job, or check status",
        argument_hint: Some("[prepare <file>|validate <file>|start|status]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature 18 — webhook manager
    SlashCommandSpec {
        name: "webhook",
        aliases: &[],
        summary: "Webhook manager — list, add, test, or remove configured endpoints",
        argument_hint: Some("[list|add <name> <url>|test <name>|remove <name>]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Feature 20 — plugin SDK
    SlashCommandSpec {
        name: "plugin-sdk",
        aliases: &[],
        summary: "Plugin SDK — scaffold, build, test, or publish an Anvil plugin",
        argument_hint: Some("[init <name>|build|test|publish]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
    },
    // Screensaver / burn-in protection
    SlashCommandSpec {
        name: "sleep",
        aliases: &["screensaver", "furnace"],
        summary: "Activate the furnace screensaver (also triggers after 15 min idle)",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",
    },
    // Fast mode toggle
    SlashCommandSpec {
        name: "fast",
        aliases: &[],
        summary: "Toggle fast mode (lower max_tokens, concise system prompt prefix)",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "\
/fast — Toggle fast mode

When fast mode is ON:
  - max_tokens is reduced to 1024 (shorter responses)
  - \"Be concise and direct.\" is prepended to the system prompt

Useful for quick questions and lookups where a brief answer is preferred.
Toggle again to restore normal mode.",
    },
    // GitHub PR review
    SlashCommandSpec {
        name: "review-pr",
        aliases: &[],
        summary: "Fetch a GitHub PR diff and send to AI for a code review",
        argument_hint: Some("[<pr-number>]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "\
/review-pr — AI-powered GitHub pull request review

Usage:
  /review-pr           Review the PR associated with the current branch
  /review-pr <number>  Review a specific PR by number

Fetches the diff via `gh pr diff` and the PR metadata via `gh pr view`,
then sends both to the AI for a comprehensive code review covering:
  - Bugs and logic errors
  - Security vulnerabilities
  - Performance concerns
  - Style and readability
  - Suggested improvements

Requires the `gh` CLI to be installed and authenticated.

Examples:
  /review-pr
  /review-pr 42",
    },
];

#[must_use]
pub fn slash_command_specs() -> &'static [SlashCommandSpec] {
    SLASH_COMMAND_SPECS
}

#[must_use]
pub fn resume_supported_slash_commands() -> Vec<&'static SlashCommandSpec> {
    slash_command_specs()
        .iter()
        .filter(|spec| spec.resume_supported)
        .collect()
}

#[must_use]
pub fn render_slash_command_help() -> String {
    let mut lines = vec![
        "Slash commands".to_string(),
        "  Tab completes commands inside the REPL.".to_string(),
        "  [resume] = also available via anvil --resume SESSION.json".to_string(),
    ];

    for category in [
        SlashCommandCategory::Core,
        SlashCommandCategory::Workspace,
        SlashCommandCategory::Session,
        SlashCommandCategory::Git,
        SlashCommandCategory::Automation,
    ] {
        lines.push(String::new());
        lines.push(category.title().to_string());
        lines.extend(
            slash_command_specs()
                .iter()
                .filter(|spec| spec.category == category)
                .map(render_slash_command_entry),
        );
    }

    lines.join("\n")
}

fn render_slash_command_entry(spec: &SlashCommandSpec) -> String {
    let alias_suffix = if spec.aliases.is_empty() {
        String::new()
    } else {
        format!(
            " (aliases: {})",
            spec.aliases
                .iter()
                .map(|alias| format!("/{alias}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let resume = if spec.resume_supported {
        " [resume]"
    } else {
        ""
    };
    format!(
        "  {name:<46} {}{alias_suffix}{resume}",
        spec.summary,
        name = render_slash_command_name(spec),
    )
}

fn render_slash_command_name(spec: &SlashCommandSpec) -> String {
    match spec.argument_hint {
        Some(argument_hint) => format!("/{} {}", spec.name, argument_hint),
        None => format!("/{}", spec.name),
    }
}

/// Look up the detailed help for a named command.
///
/// Returns `Some(text)` when the command is found and has non-empty
/// `detailed_help`, or `None` if the command is unknown or has no detailed
/// help.
#[must_use]
pub fn render_command_detailed_help(command_name: &str) -> Option<String> {
    let name = command_name.trim_start_matches('/');
    slash_command_specs()
        .iter()
        .find(|spec| spec.name == name || spec.aliases.contains(&name))
        .filter(|spec| !spec.detailed_help.is_empty())
        .map(|spec| spec.detailed_help.to_string())
}

fn levenshtein_distance(left: &str, right: &str) -> usize {
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
            let cost = usize::from(left_char != *right_char);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[right_chars.len()]
}

#[must_use]
pub fn suggest_slash_commands(input: &str, limit: usize) -> Vec<String> {
    let normalized = input.trim().trim_start_matches('/').to_ascii_lowercase();
    if normalized.is_empty() || limit == 0 {
        return Vec::new();
    }

    let mut ranked = slash_command_specs()
        .iter()
        .filter_map(|spec| {
            let score = std::iter::once(spec.name)
                .chain(spec.aliases.iter().copied())
                .map(str::to_ascii_lowercase)
                .filter_map(|alias| {
                    if alias == normalized {
                        Some((0_usize, alias.len()))
                    } else if alias.starts_with(&normalized) {
                        Some((1, alias.len()))
                    } else if alias.contains(&normalized) {
                        Some((2, alias.len()))
                    } else {
                        let distance = levenshtein_distance(&alias, &normalized);
                        (distance <= 2).then_some((3 + distance, alias.len()))
                    }
                })
                .min();

            score.map(|(bucket, len)| (bucket, len, render_slash_command_name(spec)))
        })
        .collect::<Vec<_>>();

    ranked.sort();
    ranked.dedup_by(|left, right| left.2 == right.2);
    ranked
        .into_iter()
        .take(limit)
        .map(|(_, _, display)| display)
        .collect()
}
