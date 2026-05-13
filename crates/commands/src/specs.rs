use crate::subcommands::{RestartRequirement, SubcommandSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommandCategory {
    Core,
    Workspace,
    Session,
    Git,
    Automation,
}

impl SlashCommandCategory {
    pub const fn title(self) -> &'static str {
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
    // ── v2.2.6 additions ─────────────────────────────────────────────────────
    /// Hierarchical subcommand tree. Empty slice for leaf commands.
    pub subcommands: &'static [SubcommandSpec],
    /// Whether this command is implemented in TUI mode.
    pub tui_available: bool,
    /// Whether this command is invokable via the web viewer.
    pub web_available: bool,
    /// Whether the vault must be unlocked before this command can run.
    pub requires_vault: bool,
    /// How much restart is needed after the command completes.
    pub requires_restart: RestartRequirement,
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
        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "status",
        aliases: &[],
        summary: "Show current session status",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "compact",
        aliases: &[],
        summary: "Compact local session history",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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
        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "permissions",
        aliases: &[],
        summary: "Show or switch the active permission mode",
        argument_hint: Some("[read-only|workspace-write|danger-full-access]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",

        subcommands: crate::subcommands::PERMISSIONS_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "clear",
        aliases: &[],
        summary: "Start a fresh local session",
        argument_hint: Some("[--confirm]"),
        resume_supported: true,
        category: SlashCommandCategory::Session,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "cost",
        aliases: &["usage", "stats"],
        summary: "Show cumulative token usage for this session",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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
        subcommands: crate::subcommands::PROVIDER_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "login",
        aliases: &[],
        summary: "Login or refresh credentials for a provider",
        argument_hint: Some("[anthropic|openai|ollama]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",

        subcommands: crate::subcommands::LOGIN_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "resume",
        aliases: &[],
        summary: "Load a saved session into the REPL",
        argument_hint: Some("<session-path>"),
        resume_supported: false,
        category: SlashCommandCategory::Session,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "config",
        aliases: &[],
        summary: "Inspect Anvil config files or merged sections",
        argument_hint: Some("[env|hooks|model|plugins]"),
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",

        subcommands: crate::subcommands::CONFIG_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "memory",
        aliases: &[],
        summary: "Inspect and manage all memory tiers (ANVIL.md, vault, nominations, cache, \u{2026})",
        argument_hint: Some("[show|inspect|promote|forget|why|budget|prune] [arg]"),
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "\
/memory — memory tier inspector and manager

SUBCOMMANDS
  (none)              Print a one-line count for every active memory tier
  show <tier> [sub]   Dump contents of a specific tier, with optional sub-view
  inspect <key>       Search every searchable tier (incl. L7 caches) for <key>
  promote [--dry-run] <id>
                      Accept a pending nomination into ANVIL.md.
                      --dry-run shows the planned write without committing.
  forget <key>        Remove an entry from ANVIL.md (or reject a nomination)
  why                 List the LIVE system_prompt sections in injection order.
                      Reads ONLY the current working-memory snapshot (L1).
                      Does NOT walk DailyStore (L2) or reconcile across tiers.
                      For tier-by-tier tables, use /memory show <tier>.
  budget              Per-tier byte/~token table including the working row
  prune               Remove stale entries from daily/nominations tiers

TIERS  (seven-layer vocabulary, Phase 2 of the Memory Cohesion Arc)
  working             L1 — live system_prompt sections + message-buffer stats
  episodic            L2 — archived sessions + `episodic daily` sub-view
  semantic            L3 — anvil-md (approved) + `semantic --pending` nominations
  procedural          L4 — `procedural {goals|skills|cron|routines}`
  identity            L5 — vault labels (unlocked) or counts only (locked)
  policy              L6 — PermissionMemory grants + auto-mode + reviewer + egress
  cache               L7 — `cache {file|cmd|qmd}` token-economy caches

LEGACY TIER NAMES  (kept one cycle; Phase 4 will redirect them)
  anvil-md            ANVIL.md / MEMORY.md (L3 approved half)
  vault               see `identity`
  private             AES-encrypted per-project memory (vault-locked)
  nominations         (deprecated alias for `semantic --pending`)
  daily               (alias for `episodic daily`)
  file-cache          (alias for `cache file`)
  cmd-cache           (alias for `cache cmd`)
  goals               (alias for `procedural goals`)

EXAMPLES
  /memory show working
  /memory show episodic daily
  /memory show semantic --pending
  /memory show procedural skills
  /memory show identity
  /memory show policy
  /memory show cache qmd
  /memory inspect deploy
  /memory budget
  /memory prune
",

        subcommands: crate::subcommands::MEMORY_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "init",
        aliases: &[],
        summary: "Create a starter ANVIL.md for this repo",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "diff",
        aliases: &[],
        summary: "Show git diff for current workspace changes",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "version",
        aliases: &[],
        summary: "Show CLI version and build information",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "bughunter",
        aliases: &[],
        summary: "Inspect the codebase for likely bugs",
        argument_hint: Some("[scope]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "branch",
        aliases: &[],
        summary: "List, create, or switch git branches",
        argument_hint: Some("[list|create <name>|switch <name>]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",

        subcommands: crate::subcommands::BRANCH_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "worktree",
        aliases: &[],
        summary: "List, add, remove, or prune git worktrees",
        argument_hint: Some("[list|add <path> [branch]|remove <path>|prune]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",

        subcommands: crate::subcommands::WORKTREE_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "commit",
        aliases: &[],
        summary: "Generate a commit message and create a git commit",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "commit-push-pr",
        aliases: &[],
        summary: "Commit workspace changes, push the branch, and open a PR",
        argument_hint: Some("[context]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "pr",
        aliases: &[],
        summary: "Draft or create a pull request from the conversation",
        argument_hint: Some("[context]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "issue",
        aliases: &[],
        summary: "Draft or create a GitHub issue from the conversation",
        argument_hint: Some("[context]"),
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "ultraplan",
        aliases: &[],
        summary: "Run a deep planning prompt with multi-step reasoning",
        argument_hint: Some("[task]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "teleport",
        aliases: &[],
        summary: "Jump to a file or symbol by searching the workspace",
        argument_hint: Some("<symbol-or-path>"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "debug-tool-call",
        aliases: &[],
        summary: "Replay the last tool call with debug details",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "export",
        aliases: &[],
        summary: "Export the current conversation to a file",
        argument_hint: Some("[file]"),
        resume_supported: true,
        category: SlashCommandCategory::Session,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "session",
        aliases: &[],
        summary: "List or switch managed local sessions",
        argument_hint: Some("[list|switch <session-id>]"),
        resume_supported: false,
        category: SlashCommandCategory::Session,
        detailed_help: "",

        subcommands: crate::subcommands::SESSION_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::PLUGINS_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "agents",
        aliases: &[],
        summary: "List configured agents",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Automation,
        detailed_help: "",

        subcommands: crate::subcommands::AGENTS_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "skills",
        aliases: &[],
        summary: "List available skills",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Automation,
        detailed_help: "",

        subcommands: crate::subcommands::SKILLS_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "qmd",
        aliases: &[],
        summary: "Search the local markdown knowledge base via QMD",
        argument_hint: Some("<query>"),
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "undo",
        aliases: &[],
        summary: "Undo last file change (unstaged diff or last commit)",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Git,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "history",
        aliases: &[],
        summary: "Show conversation history (last 20 messages)",
        argument_hint: Some("[all]"),
        resume_supported: true,
        category: SlashCommandCategory::Session,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "context",
        aliases: &[],
        summary: "Add a file to context, or list context files",
        argument_hint: Some("[path]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "pin",
        aliases: &[],
        summary: "Pin a file to always-in-context (persists across sessions)",
        argument_hint: Some("[path]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "unpin",
        aliases: &[],
        summary: "Remove a pinned file",
        argument_hint: Some("<path>"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "chat",
        aliases: &[],
        summary: "Toggle chat-only mode (disables all tools)",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "vim",
        aliases: &[],
        summary: "Toggle vim keybindings in the input editor",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "web",
        aliases: &[],
        summary: "Quick web search — results shown inline without a model turn",
        argument_hint: Some("<query>"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "doctor",
        aliases: &[],
        summary: "Diagnose Anvil configuration and dependencies",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "tokens",
        aliases: &[],
        summary: "Detailed per-turn and cumulative token breakdown with cost estimate",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "search",
        aliases: &[],
        summary: "Multi-provider web search (duckduckgo, tavily, brave, exa, …)",
        argument_hint: Some("[provider <name>] <query> | providers | config <key> <val>"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",

        subcommands: crate::subcommands::SEARCH_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "failover",
        aliases: &[],
        summary: "Manage AI provider failover chain (rate-limit handling)",
        argument_hint: Some("[status | add <model> | remove <model> | reset]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",

        subcommands: crate::subcommands::FAILOVER_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "generate-image",
        aliases: &["image"],
        summary: "Generate an image with OpenAI and download it locally",
        argument_hint: Some("[--wp <post-id>] <prompt>"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "history-archive",
        aliases: &[],
        summary: "Browse, search, or view archived session history",
        argument_hint: Some("[search <query> | view <session-id>]"),
        resume_supported: true,
        category: SlashCommandCategory::Session,
        detailed_help: "",

        subcommands: crate::subcommands::HISTORY_ARCHIVE_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "configure",
        aliases: &["settings", "config-menu"],
        summary: "Interactive configuration wizard — providers, models, context, search, …",
        argument_hint: Some("[providers|models|context|search|permissions|display|integrations]"),
        resume_supported: true,
        category: SlashCommandCategory::Core,
        detailed_help: "",

        subcommands: crate::subcommands::CONFIGURE_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "theme",
        aliases: &[],
        summary: "Show, list, or change the TUI colour theme; create/import/export custom themes",
        argument_hint: Some("[list | set <name> | reset | create <name> | import <file> | export <name>]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",

        subcommands: crate::subcommands::THEME_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    // Feature 3 — semantic code search
    SlashCommandSpec {
        name: "semantic-search",
        aliases: &["symsearch"],
        summary: "Search for symbols (functions, classes, structs, imports) grouped by type",
        // NOTE: hint uses commas inside the `--type` value list (not pipes) so
        // the drift-prevention test does not flag this as a subcommand-bearing
        // hint. /semantic-search has no subcommands — the dispatch in
        // `cmd_static::run_semantic_search` is a free-form query plus optional
        // `--lang` and `--type` flags. Picker enumeration would mislead users
        // into thinking `fn`/`class`/etc. are top-level subcommands.
        argument_hint: Some("<query> [--lang <ext>] [--type fn,class,struct,import]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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
        subcommands: crate::subcommands::DOCKER_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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
        subcommands: crate::subcommands::TEST_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::GIT_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::REFACTOR_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::DB_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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
        subcommands: crate::subcommands::SECURITY_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::API_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::DOCS_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::SCAFFOLD_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::PERF_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::DEBUG_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::VOICE_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::COLLAB_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::ENV_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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
        subcommands: crate::subcommands::HUB_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: true, // install action requires unlocked vault
        requires_restart: RestartRequirement::Full, // installing plugins/MCP servers needs restart
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

        subcommands: crate::subcommands::LANGUAGE_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::LSP_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::NOTEBOOK_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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
        subcommands: crate::subcommands::K8S_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::IAC_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::PIPELINE_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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
        subcommands: crate::subcommands::REVIEW_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::DEPS_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::MONO_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::BROWSER_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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
        subcommands: crate::subcommands::NOTIFY_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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
        subcommands: crate::subcommands::VAULT_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false, // setup/unlock don't need vault, others checked at runtime
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::MIGRATE_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::REGEX_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::SSH_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::LOGS_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::MARKDOWN_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::SNIPPETS_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::FINETUNE_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::WEBHOOK_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: crate::subcommands::PLUGIN_SDK_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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

        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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
        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
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
        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    // ── Commands present in the enum but previously missing from specs ────────
    SlashCommandSpec {
        name: "mcp",
        aliases: &[],
        summary: "MCP server management — list, status, tools",
        argument_hint: Some("[list|status|tools <server>]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
        subcommands: crate::subcommands::MCP_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "productivity",
        aliases: &[],
        summary: "Show session productivity statistics",
        argument_hint: None,
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "knowledge",
        aliases: &["nominations"],
        summary: "Manage knowledge nominations — review, accept, reject, list",
        argument_hint: Some("[review|accept <N>|reject <N>|list]"),
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
        subcommands: crate::subcommands::KNOWLEDGE_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "daily",
        aliases: &["summary"],
        summary: "View daily summary and task reconciliation",
        argument_hint: Some("[date]"),
        resume_supported: true,
        category: SlashCommandCategory::Workspace,
        detailed_help: "",
        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "think",
        aliases: &["thinking", "nothink"],
        summary: "Toggle thinking/reasoning mode (for models that support it)",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",
        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "focus",
        aliases: &[],
        summary: "Toggle focus view (prompt + tool summary + final response only)",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",
        subcommands: &[],
        tui_available: true,
        web_available: false,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "loop",
        aliases: &["proactive"],
        summary: "Autonomous loop mode — run a prompt repeatedly until done",
        argument_hint: Some("[prompt]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "",
        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "remote-control",
        aliases: &["rc"],
        summary: "Start (or stop/query) a web viewer relay session",
        argument_hint: Some("[stop|status]"),
        resume_supported: false,
        category: SlashCommandCategory::Session,
        detailed_help: "",
        subcommands: crate::subcommands::REMOTE_CONTROL_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    // ── Ghost commands promoted to real variants (v2.2.6 Phase 0) ────────────
    SlashCommandSpec {
        name: "tab",
        aliases: &[],
        summary: "Tab management — new, close, switch, list",
        argument_hint: Some("[new|close|switch <id>|list]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "\
/tab — Manage TUI tabs

Subcommands:
  /tab new            Open a new tab
  /tab close          Close the current tab
  /tab switch <id>    Switch to a tab by index or ID
  /tab list           List all open tabs",
        subcommands: crate::subcommands::TAB_SUBCOMMANDS,
        tui_available: true,
        web_available: false,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "fork",
        aliases: &[],
        summary: "Duplicate the current tab with the same conversation context",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "",
        subcommands: &[],
        tui_available: true,
        web_available: false,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "share",
        aliases: &[],
        summary: "Share the current tab as a read-only link (24h expiry)",
        argument_hint: Some("[stop|list]"),
        resume_supported: false,
        category: SlashCommandCategory::Session,
        detailed_help: "\
/share — Share the current tab's conversation as a read-only link

Usage:
  /share         Generate a read-only share URL for the current tab
  /share stop    Revoke the active share
  /share list    List active shares

Notes:
  - Requires vault to be unlocked (anti-abuse)
  - Shares expire after 24 hours by default
  - Viewers see messages only — they cannot send input
  - Distinct from /remote-control (which shares full Anvil control)",
        subcommands: crate::subcommands::SHARE_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: true,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "audit",
        aliases: &[],
        summary: "Composite security audit: /security scan + /deps audit + /vault verify",
        argument_hint: None,
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "\
/audit — Composite security audit

Runs the following in sequence:
  1. /security scan   — Static analysis for vulnerabilities
  2. /deps audit      — Dependency CVE scan
  3. /vault verify    — Vault integrity check

Results are combined into a single report.",
        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    // ── Respawn mechanism (v2.2.6 Phase 5) ─────────────────────────────────
    SlashCommandSpec {
        name: "restart",
        aliases: &[],
        summary: "Restart Anvil — full respawn or soft config reload",
        argument_hint: Some("[--soft]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "\
/restart — Restart Anvil

Usage:
  /restart          Full process restart (prompts for confirmation)
  /restart --soft   Reload configuration without restarting the process

Notes:
  - Full restart uses execvp(2) on macOS/Linux to replace the current process in-place.
  - On Windows or when launched via pipe/cargo/debugger, /restart prints the command
    to run manually and exits with code 42.
  - /restart --soft is safe in all environments and only reloads config.",
        subcommands: &[],
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::Full,
    },
    // ── Agent composition (v2.2.7) ────────────────────────────────────────────
    SlashCommandSpec {
        name: "agent",
        aliases: &[],
        summary: "Compose a trait-based agent for a one-shot task",
        argument_hint: Some("[traits|compose <traits> \"<task>\"]"),
        resume_supported: false,
        category: SlashCommandCategory::Automation,
        detailed_help: "\
/agent — Trait-based agent composition

Usage:
  /agent traits                              List available traits
  /agent compose <trait1,trait2> \"<task>\"   Compose and run a one-shot agent",
        subcommands: crate::subcommands::AGENT_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    // ── Skill dispatch (v2.2.7) ──────────────────────────────────────────────
    SlashCommandSpec {
        name: "skill",
        aliases: &[],
        summary: "Load, suggest, or list Anvil skills",
        argument_hint: Some("[suggest [<prompt>]|load <name>|list|chains]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "\
/skill — Anvil skill management

Usage:
  /skill list               List all available skills
  /skill load <name>        Prepend skill body to the next system prompt turn
  /skill suggest [<prompt>] Show trigger-matched skill suggestions for a prompt
  /skill chains             Show the skill chain graph (skills with chains_to entries)",
        subcommands: crate::subcommands::SKILL_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "output-style",
        aliases: &["output_style"],
        summary: "Set response style: built-in or custom (~/.anvil/output-styles/)",
        argument_hint: Some("[<name>|list|reset]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "\
/output-style — Control response verbosity and style

Usage:
  /output-style             Show the current output style
  /output-style list        List all available styles (built-ins + user styles)
  /output-style reset       Reset to the default style (precise)
  /output-style precise     Natural model voice — no extra instructions (default)
  /output-style condensed   Terse, bullet-point responses (opt-in)
  /output-style <name>      Activate a user-defined style

Custom styles:
  Place Markdown files in ~/.anvil/output-styles/<name>.md with YAML frontmatter:

    ---
    name: Tutor
    description: Explanatory style with code commentary
    ---

    You are a patient teacher. After every code block, explain what changed.

  The frontmatter 'name' and 'description' fields are required.
  The body becomes the system prompt fragment prepended for each turn.
  If a user style has the same name as a built-in, the user file wins.",
        subcommands: crate::subcommands::OUTPUT_STYLE_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "goal",
        aliases: &[],
        summary: "Manage long-running goals across sessions",
        argument_hint: Some("[new|list|resume|pause|done|show]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "\
/goal — Per-project goal persistence

Usage:
  /goal                            List all goals (alias for /goal list)
  /goal new \"<description>\"       Create a goal, set it active (max 4096 chars)
  /goal list                       Show all goals: active first, then paused, done
  /goal resume <id>                Mark a goal active; auto-pauses current active
  /goal pause [<id>]               Pause the active goal (or a specific one by id)
  /goal done [<id>]                Mark active (or specified) goal done; file kept
  /goal show [<id>]                Print full goal details and linked sessions

One active goal at a time per project. Goals are scoped to the current
project directory and persist across sessions in ~/.anvil/goals/.",
        subcommands: crate::subcommands::GOAL_SUBCOMMANDS,
        tui_available: true,
        web_available: false,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    // W4 — Named profiles
    SlashCommandSpec {
        name: "profile",
        aliases: &[],
        summary: "Manage named config profiles (work, personal, client-A, …)",
        argument_hint: Some("[list|use <name>|show [<name>]|create <name>|delete <name>]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "\
/profile — Named configuration profiles

Usage:
  /profile list             List all profiles; marks the active one
  /profile use <name>       Switch active profile for this session only
  /profile show [<name>]    Print fields of a profile (defaults to active)
  /profile create <name>    Create an empty profile inheriting current config
  /profile delete <name>    Remove a named profile

Notes:
  Profile fields override base config. Unset fields fall through to base.
  Use --profile <name> at startup for a persistent selection.
  ANVIL_PROFILE env var also sets the active profile (lower precedence than --profile).

Overridable fields per profile:
  model, provider, effort_level, output_style, permission_mode,
  enabled_plugins, enabled_mcp_servers, env",
        subcommands: crate::subcommands::PROFILE_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "effort",
        aliases: &[],
        summary: "Get or set the per-session reasoning effort level",
        argument_hint: Some("[low|medium|high|xhigh]"),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "\
/effort — Control model reasoning effort per session

Usage:
  /effort              Show the current effort level
  /effort low          Minimal reasoning tokens (fast, cheap)
  /effort medium       Balanced reasoning — default
  /effort high         Deep reasoning (slower, higher quality)
  /effort xhigh        Maximum reasoning budget (slowest, highest quality)

Provider mapping:
  Anthropic (Claude)   thinking.budget_tokens: low=2k, medium=8k, high=24k, xhigh=64k
  OpenAI / xAI         reasoning.effort: low|medium|high  (xhigh → high)
  Gemini               thinkingBudget: low=2k, medium=8k, high=24k, xhigh=-1 (dynamic)
  Ollama / local       no-op for model; sets ANVIL_EFFORT env var for hooks and MCP servers

The session override applies for all subsequent turns until /effort is called again
or the session ends. Set a persistent default in settings.json with:
  { \"effort_level\": \"high\" }",
        subcommands: crate::subcommands::EFFORT_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "ollama",
        aliases: &[],
        summary: "Read + manage local Ollama models (list, show, ps, tune, ...)",
        argument_hint: Some(
            "[list|show <model>|ps|tune <model>|option <model> <k> <v>|policy <model> <k> <v>|pull <model>|rm <model>|cp <src> <dst>|create <model>|bench [<model>]|requantize <model>]",
        ),
        resume_supported: false,
        category: SlashCommandCategory::Core,
        detailed_help: "\
/ollama \u{2014} local Ollama model inspection and management

USAGE
  /ollama list                          List installed models
  /ollama show <model>                  Show /api/show details for a model
  /ollama ps                            Show currently loaded models (/api/ps)
  /ollama tune <model>                  Compute recommended OllamaOptions for
                                        this hardware + policy
  /ollama option <model> <key> <value>  Override a runtime option
                                        (num_ctx, num_predict, temperature, \u{2026})
  /ollama policy <model> <key> <value>  Update tuner policy fields
                                        (vram_target, quant_floor, \u{2026})
  /ollama pull <model>                  Download a model (confirmation prompt)
  /ollama rm <model>                    Remove an installed model (prompt)
  /ollama cp <src> <dst>                Copy a model to a new tag
  /ollama create <model>                Create a model from a Modelfile
  /ollama bench [<model>]               Run the benchmark harness against a model
                                        (defaults to the active model)
  /ollama requantize <model>            Helper to suggest/execute a re-quantization

EXAMPLES
  /ollama list
  /ollama show qwen3:8b
  /ollama tune qwen3:8b
  /ollama option qwen3:8b num_ctx 8192
  /ollama policy qwen3:8b vram_target 0.85
  /ollama bench qwen3:8b
",
        subcommands: crate::subcommands::OLLAMA_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "file-cache",
        aliases: &["fc"],
        summary: "Inspect and manage the file-fingerprint cache (W11 token economy)",
        argument_hint: Some("[stats|list|forget <path>|prune|clear [--yes]|help]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "\
/file-cache \u{2014} file-fingerprint cache (W11 token economy)

USAGE
  /file-cache               Show help (default)
  /file-cache help          Show help
  /file-cache stats         Aggregate stats (entries, bytes, hit rate)
  /file-cache list          List cached files with hit counts
  /file-cache forget <path> Drop the cache entry for a specific file
  /file-cache prune         Remove stale entries (deleted or changed files)
  /file-cache clear         Wipe the entire cache (confirmation prompt)
  /file-cache clear --yes   Wipe without confirmation

EXAMPLES
  /file-cache stats
  /file-cache forget crates/runtime/src/lib.rs
  /file-cache prune
  /file-cache clear --yes
",
        subcommands: crate::subcommands::FILE_CACHE_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "cmd-cache",
        aliases: &["cc"],
        summary: "Inspect and manage the command-output cache (W12 token economy)",
        argument_hint: Some("[stats|list|forget <command>|prune-stale|clear [--yes]|help]"),
        resume_supported: false,
        category: SlashCommandCategory::Workspace,
        detailed_help: "\
/cmd-cache \u{2014} command-output cache (W12 token economy)

USAGE
  /cmd-cache                   Show help (default)
  /cmd-cache help              Show help
  /cmd-cache stats             Aggregate stats (entries, bytes, hit rate)
  /cmd-cache list              List cached commands with hit counts
  /cmd-cache forget <command>  Drop cached output for a specific command
  /cmd-cache prune-stale       Remove stale or expired entries
  /cmd-cache clear             Wipe the entire cache (confirmation prompt)
  /cmd-cache clear --yes       Wipe without confirmation

EXAMPLES
  /cmd-cache stats
  /cmd-cache forget \"cargo test\"
  /cmd-cache prune-stale
  /cmd-cache clear --yes
",
        subcommands: crate::subcommands::CMD_CACHE_SUBCOMMANDS,
        tui_available: true,
        web_available: true,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
    SlashCommandSpec {
        name: "scroll-speed",
        aliases: &["scroll_speed", "scrollspeed"],
        summary: "Set mouse-wheel scroll speed (lines per tick) — CC-139-F3 parity",
        argument_hint: Some("[1..=10]"),
        resume_supported: false,
        category: SlashCommandCategory::Session,
        detailed_help: "/scroll-speed — Mouse-wheel scroll speed (CC-139-F3)\n\nUsage:\n  /scroll-speed           Show the current value\n  /scroll-speed N         Set to N lines per wheel tick (1..=10, default 3)\n\nThe value is process-scoped. Persist a default in settings.json:\n  { \"scroll_speed\": 5 }",
        subcommands: &[],
        tui_available: true,
        web_available: false,
        requires_vault: false,
        requires_restart: RestartRequirement::None,
    },
];

#[must_use]
pub const fn slash_command_specs() -> &'static [SlashCommandSpec] {
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

// ── Deep hierarchical completion (v2.2.6 Phase 0) ────────────────────────────

/// Walk the spec tree for `input` and return the next-level completions.
///
/// # Behaviour
/// - `input` may or may not have a leading `/`.
/// - A **trailing space** means the user finished the current token and wants
///   the next argument's completions.
/// - **No trailing space** means the user is still typing the current token;
///   the current partial token is used to filter the completions.
///
/// Dynamic enum sources are resolved via `ctx`.  Pass [`NoopCompletionContext`]
/// for offline/test use.
///
/// # Examples
/// ```
/// use commands::specs::suggest_completions;
/// use commands::subcommands::NoopCompletionContext;
/// let ctx = NoopCompletionContext;
/// let cs = suggest_completions("/vault ", &ctx);
/// assert!(cs.iter().any(|c| c.text == "store"));
/// ```
#[must_use]
pub fn suggest_completions(
    input: &str,
    ctx: &dyn crate::subcommands::CompletionContext,
) -> Vec<crate::subcommands::Completion> {
    use crate::subcommands::Completion;

    let raw = input.trim_start_matches('/');
    let trailing_space = input.ends_with(' ');

    let mut tokens: Vec<&str> = raw.split_whitespace().collect();

    // If trailing space, all tokens are complete and we want the next slot.
    // If no trailing space, the last token is the partial being typed.
    let partial: String = if trailing_space || tokens.is_empty() {
        String::new()
    } else {
        tokens.pop().unwrap_or("").to_ascii_lowercase()
    };

    // ── Stage 1: root command not yet typed ──────────────────────────────────
    if tokens.is_empty() {
        return slash_command_specs()
            .iter()
            .filter(|spec| {
                partial.is_empty()
                    || spec.name.starts_with(partial.as_str())
                    || spec.aliases.iter().any(|a| a.starts_with(partial.as_str()))
            })
            .map(|spec| Completion {
                text: format!("/{}", spec.name),
                description: spec.summary.to_string(),
                category: Some(spec.category),
            })
            .collect();
    }

    // ── Stage 2: resolve the root command ────────────────────────────────────
    let root_name = tokens[0].to_ascii_lowercase();
    let Some(spec) = slash_command_specs().iter().find(|s| {
        s.name == root_name || s.aliases.iter().any(|a| *a == root_name)
    }) else {
        return vec![];
    };

    // Remaining tokens after the command name
    let rest = &tokens[1..];

    // ── Stage 2b: /model gets live provider-aware completions ────────────────
    //
    // `/model` has no subcommands (it's `/model <name>`), but the user expects
    // to see every model from every configured provider when they hit TAB.
    // The static spec tree can't represent "list all models, grouped by
    // provider" so we route through `CompletionContext::model_choices()` here.
    //
    // This was the regression behind task #374 — when the registry-driven
    // completion replaced `subcommands_for("/model")` in tui.rs, the /model
    // arm fell through to the empty `subcommands: &[]` and returned nothing.
    //
    // We only intercept when `rest` is empty: once the user has typed a
    // partial model name (e.g. `/model opus`), we still substring-match
    // through the same path so the picker filters live.
    if spec.name == "model" && rest.len() <= 1 {
        // If `rest.len() == 1` and no trailing space, the spec walker has
        // already stripped that token into `partial`; if `rest.len() == 1`
        // *with* trailing space, we treat it as past-the-arg and return empty
        // (mirroring the no-second-positional contract).
        if trailing_space && !rest.is_empty() {
            return vec![];
        }
        let choices = ctx.model_choices();
        if !choices.is_empty() {
            let needle = partial.as_str();
            return choices
                .into_iter()
                .filter(|(name, _)| {
                    needle.is_empty() || name.to_ascii_lowercase().contains(needle)
                })
                .map(|(name, provider)| Completion {
                    text: name,
                    description: provider,
                    category: Some(spec.category),
                })
                .collect();
        }
        // Fall back to walking the (empty) subcommand tree so the test
        // assertion that /model returns no completions when the context
        // doesn't supply any still holds.
    }

    // ── Stage 3: walk the subcommand tree ────────────────────────────────────
    walk_subcommands(spec.subcommands, spec.category, rest, &partial, ctx)
}

/// Recursively walk the subcommand tree.
///
/// `path` is the slice of fully-typed tokens *after* the current tree level's
/// name was consumed.  `partial` is the partial token being typed (or `""`).
fn walk_subcommands(
    subs: &'static [crate::subcommands::SubcommandSpec],
    category: SlashCommandCategory,
    path: &[&str],
    partial: &str,
    ctx: &dyn crate::subcommands::CompletionContext,
) -> Vec<crate::subcommands::Completion> {
    use crate::subcommands::Completion;

    // No more typed tokens — show completions at this level
    if path.is_empty() {
        if subs.is_empty() {
            return vec![];
        }
        return subs
            .iter()
            .filter(|s| partial.is_empty() || s.name.starts_with(partial))
            .map(|s| Completion {
                text: s.name.to_string(),
                description: s.summary.to_string(),
                category: Some(category),
            })
            .collect();
    }

    // Try to match the first token to a known subcommand
    let head = path[0];
    let tail = &path[1..];

    if let Some(matched) = subs.iter().find(|s| s.name == head) {
        // If matched sub has further subcommands, recurse
        if !matched.subcommands.is_empty() {
            return walk_subcommands(matched.subcommands, category, tail, partial, ctx);
        }
        // Otherwise walk the arg specs
        return walk_args(matched.args, category, tail, partial, ctx);
    }

    // Head didn't match a subcommand name — nothing to suggest at this point
    vec![]
}

/// Expand the next [`ArgSpec`] slot for `remaining_path` and `partial`.
fn walk_args(
    args: &'static [crate::subcommands::ArgSpec],
    category: SlashCommandCategory,
    remaining_path: &[&str],
    partial: &str,
    ctx: &dyn crate::subcommands::CompletionContext,
) -> Vec<crate::subcommands::Completion> {
    use crate::subcommands::{ArgSpec, ArgSpecValue, Completion};

    let index = remaining_path.len(); // how many arg slots already filled
    let Some(arg_spec) = args.get(index) else {
        return vec![];
    };

    match arg_spec {
        ArgSpec::Literal(lit) => {
            if partial.is_empty() || lit.starts_with(partial) {
                vec![Completion {
                    text: (*lit).to_string(),
                    description: String::new(),
                    category: Some(category),
                }]
            } else {
                vec![]
            }
        }
        ArgSpec::OneOf(choices) => choices
            .iter()
            .filter(|c| partial.is_empty() || c.starts_with(partial))
            .map(|c| Completion {
                text: (*c).to_string(),
                description: String::new(),
                category: Some(category),
            })
            .collect(),
        ArgSpec::DynamicEnum(source) => ctx
            .resolve(*source)
            .into_iter()
            .filter(|v| partial.is_empty() || v.starts_with(partial))
            .map(|v| Completion {
                text: v,
                description: String::new(),
                category: Some(category),
            })
            .collect(),
        ArgSpec::FreeText { hint } => {
            if partial.is_empty() {
                vec![Completion {
                    text: format!("<{hint}>"),
                    description: "free-form text".to_string(),
                    category: Some(category),
                }]
            } else {
                vec![]
            }
        }
        ArgSpec::OptionalFlag { name, value } => {
            if partial.is_empty() || name.starts_with(partial) {
                let desc = match value {
                    None => "flag".to_string(),
                    Some(ArgSpecValue::FreeText { hint }) => format!("<{hint}>"),
                    Some(ArgSpecValue::OneOf(choices)) => choices.join("|"),
                    Some(ArgSpecValue::DynamicEnum(src)) => {
                        ctx.resolve(*src).join("|")
                    }
                };
                vec![Completion {
                    text: (*name).to_string(),
                    description: desc,
                    category: Some(category),
                }]
            } else {
                vec![]
            }
        }
    }
}
