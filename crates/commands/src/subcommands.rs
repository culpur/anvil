/// Deep hierarchical completion types for CommandSpec v2 (Anvil v2.2.6 — Phase 0).
///
/// These types are `const`-compatible where possible.  Dynamic enums are
/// resolved at call-time through the [`CompletionContext`] trait so the static
/// spec tree stays zero-allocation.

// ─── Arg spec ────────────────────────────────────────────────────────────────

/// Describes a single positional (or flag) argument slot in a subcommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgSpec {
    /// A fixed keyword, e.g. the literal `"add"` inside a composite hint.
    Literal(&'static str),
    /// One of a fixed set of string values, e.g. `["anthropic", "openai", "ollama"]`.
    OneOf(&'static [&'static str]),
    /// Resolved at runtime by calling [`CompletionContext::resolve`].
    DynamicEnum(DynamicEnumSource),
    /// Free-form text.  The `hint` is shown as placeholder, e.g. `<query>`.
    FreeText { hint: &'static str },
    /// An optional flag, e.g. `--confirm` or `--filter <pkg>`.
    OptionalFlag {
        name: &'static str,
        /// The flag's value argument, or `None` for boolean flags.
        value: Option<ArgSpecValue>,
    },
}

/// A value descriptor for an [`ArgSpec::OptionalFlag`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgSpecValue {
    FreeText { hint: &'static str },
    OneOf(&'static [&'static str]),
    DynamicEnum(DynamicEnumSource),
}

// ─── Dynamic enum sources ─────────────────────────────────────────────────────

/// Identifies where a dynamic completion list comes from.
/// The actual values are resolved through [`CompletionContext::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicEnumSource {
    /// 21 credential types from `runtime::vault::CredentialType`.
    VaultCredentialTypes,
    /// Installed plugins (from the plugins directory).
    InstalledPlugins,
    /// Installed themes (from the themes registry).
    InstalledThemes,
    /// Installed agent definitions.
    InstalledAgents,
    /// Installed skills.
    InstalledSkills,
    /// Connected MCP servers (from `~/.anvil/mcp.json`).
    McpServers,
    /// Recent managed sessions.
    Sessions,
    /// Available models for the current provider.
    Models,
    /// Installed local Ollama models (from /api/tags).
    InstalledOllamaModels,
    /// Configured AI providers.
    Providers,
    /// Supported i18n language codes.
    Languages,
    /// Active goal IDs for the current project (`/goal resume|pause|done|show`).
    Goals,
    /// Available output styles: built-ins plus user styles from
    /// `~/.anvil/output-styles/`. Also includes control tokens `list` and `reset`.
    OutputStyles,
    /// Named profiles from `~/.anvil/settings.json` (or project settings).
    Profiles,
}

// ─── Restart requirement ──────────────────────────────────────────────────────

/// How much restart is needed after executing a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartRequirement {
    /// No restart required.
    None,
    /// Config reload only (soft restart).
    Soft,
    /// Full process restart required (e.g. after installing a plugin).
    Full,
}

// ─── Subcommand spec ──────────────────────────────────────────────────────────

/// A node in the command hierarchy.  Can nest arbitrarily deep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubcommandSpec {
    /// The token the user types, e.g. `"store"` in `/vault store`.
    pub name: &'static str,
    /// One-line description shown in the completion popup.
    pub summary: &'static str,
    /// Positional arguments that follow this subcommand.
    pub args: &'static [ArgSpec],
    /// Nested sub-sub-commands, if any.
    pub subcommands: &'static [SubcommandSpec],
}

// ─── Completion context ───────────────────────────────────────────────────────

/// Resolves dynamic completion lists at call time.
///
/// Implement this on a TUI or CLI context that has access to the runtime.
/// A no-op implementation ([`NoopCompletionContext`]) is provided for tests and
/// offline use.
pub trait CompletionContext {
    fn resolve(&self, source: DynamicEnumSource) -> Vec<String>;
}

/// Fallback implementation — always returns an empty list.
///
/// Useful for unit tests and as the default for the web viewer before a
/// session is connected.
pub struct NoopCompletionContext;

impl CompletionContext for NoopCompletionContext {
    fn resolve(&self, _source: DynamicEnumSource) -> Vec<String> {
        vec![]
    }
}

/// A fallback implementation that returns hard-coded defaults for every
/// [`DynamicEnumSource`].  Useful for offline completion in the web viewer.
pub struct StaticDefaultCompletionContext;

impl CompletionContext for StaticDefaultCompletionContext {
    fn resolve(&self, source: DynamicEnumSource) -> Vec<String> {
        match source {
            DynamicEnumSource::VaultCredentialTypes => vec![
                "api_key".into(),
                "ssh_key".into(),
                "tls_cert".into(),
                "totp".into(),
                "database_url".into(),
                "oauth_token".into(),
                "encryption_key".into(),
                "webhook_secret".into(),
                "license_key".into(),
                "secret_text".into(),
                "username_password".into(),
                "cloud_credential".into(),
                "host_credential".into(),
                "docker_registry".into(),
                "kube_config".into(),
                "vpn_config".into(),
                "client_cert".into(),
                "signing_key".into(),
                "recovery_code".into(),
                "env_file".into(),
                "config_blob".into(),
            ],
            DynamicEnumSource::InstalledPlugins => vec![],
            DynamicEnumSource::InstalledThemes => {
                vec!["dark".into(), "light".into(), "solarized".into()]
            }
            DynamicEnumSource::InstalledAgents => vec![],
            DynamicEnumSource::InstalledSkills => vec![],
            DynamicEnumSource::McpServers => vec![],
            DynamicEnumSource::Sessions => vec![],
            DynamicEnumSource::Models => vec![
                "claude-opus-4-5".into(),
                "claude-sonnet-4-5".into(),
                "claude-haiku-4-5".into(),
                "gpt-4o".into(),
                "gpt-4o-mini".into(),
            ],
            DynamicEnumSource::InstalledOllamaModels => vec![],
            DynamicEnumSource::Providers => {
                vec!["anthropic".into(), "openai".into(), "ollama".into(), "xai".into()]
            }
            DynamicEnumSource::Languages => vec![
                "en".into(),
                "de".into(),
                "es".into(),
                "fr".into(),
                "ja".into(),
                "zh-CN".into(),
                "ru".into(),
            ],
            DynamicEnumSource::Goals => vec![],
            DynamicEnumSource::OutputStyles => vec![
                "precise".into(),
                "condensed".into(),
                "list".into(),
                "reset".into(),
            ],
            DynamicEnumSource::Profiles => vec![],
        }
    }
}

// ─── Completion result ────────────────────────────────────────────────────────

/// A single completion candidate returned by [`suggest_completions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The text to insert (or display in the popup).
    pub text: String,
    /// Short description shown alongside the candidate.
    pub description: String,
    /// The category of the root command this completion belongs to.
    pub category: Option<crate::specs::SlashCommandCategory>,
}

// ─── Subcommand tree definitions ──────────────────────────────────────────────
//
// Shared constant trees referenced from SLASH_COMMAND_SPECS in specs.rs.

/// Vault credential type tokens (snake_case, matching runtime vault module).
pub const VAULT_CREDENTIAL_TYPES: &[&str] = &[
    "api_key",
    "ssh_key",
    "tls_cert",
    "totp",
    "database_url",
    "oauth_token",
    "encryption_key",
    "webhook_secret",
    "license_key",
    "secret_text",
    "username_password",
    "cloud_credential",
    "host_credential",
    "docker_registry",
    "kube_config",
    "vpn_config",
    "client_cert",
    "signing_key",
    "recovery_code",
    "env_file",
    "config_blob",
];

pub const VAULT_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "setup",
        summary: "Initialise a new vault (prompts for master password)",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "unlock",
        summary: "Unlock the vault for this session",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "lock",
        summary: "Re-lock the vault immediately",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "store",
        summary: "Store a credential: /vault store <type> <label>",
        args: &[
            ArgSpec::OneOf(VAULT_CREDENTIAL_TYPES),
            ArgSpec::FreeText { hint: "<label>" },
        ],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "get",
        summary: "Retrieve a credential by label",
        args: &[ArgSpec::FreeText { hint: "<label>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "list",
        summary: "List all stored credential labels",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "delete",
        summary: "Delete a credential permanently",
        args: &[ArgSpec::FreeText { hint: "<label>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "totp",
        summary: "TOTP management: add / generate / list / delete",
        args: &[],
        subcommands: &[
            SubcommandSpec {
                name: "add",
                summary: "Add a TOTP secret",
                args: &[ArgSpec::FreeText { hint: "<label>" }],
                subcommands: &[],
            },
            SubcommandSpec {
                name: "list",
                summary: "List all TOTP labels",
                args: &[],
                subcommands: &[],
            },
            SubcommandSpec {
                name: "delete",
                summary: "Remove a TOTP entry",
                args: &[ArgSpec::FreeText { hint: "<label>" }],
                subcommands: &[],
            },
        ],
    },
    SubcommandSpec {
        name: "verify",
        summary: "Verify vault integrity",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "rotate",
        summary: "Re-encrypt vault with a new master password",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "export",
        summary: "Export vault contents to an encrypted backup",
        args: &[ArgSpec::FreeText { hint: "<path>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "import",
        summary: "Import an encrypted vault backup",
        args: &[ArgSpec::FreeText { hint: "<path>" }],
        subcommands: &[],
    },
];

pub const MCP_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "list",
        summary: "List configured MCP servers",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "status",
        summary: "Show connection status of all MCP servers",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "tools",
        summary: "List tools exposed by an MCP server",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::McpServers)],
        subcommands: &[],
    },
];

pub const PLUGINS_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "list",
        summary: "List installed plugins",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "enable",
        summary: "Enable a plugin by name",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::InstalledPlugins)],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "disable",
        summary: "Disable a plugin by name",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::InstalledPlugins)],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "search",
        summary: "Search for plugins",
        args: &[ArgSpec::FreeText { hint: "<query>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "install",
        summary: "Install a plugin from a path or name",
        args: &[ArgSpec::FreeText { hint: "<name-or-path>" }],
        subcommands: &[],
    },
];

pub const SESSION_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "list",
        summary: "List recent sessions",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "load",
        summary: "Load a session by path",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::Sessions)],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "save",
        summary: "Save the current session with a name",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "export",
        summary: "Export a session to a file",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
];

pub const KNOWLEDGE_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "review",
        summary: "Review pending knowledge nominations",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "accept",
        summary: "Accept nomination N",
        args: &[ArgSpec::FreeText { hint: "<N>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "reject",
        summary: "Reject nomination N",
        args: &[ArgSpec::FreeText { hint: "<N>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "list",
        summary: "List all nominations",
        args: &[],
        subcommands: &[],
    },
];

pub const HUB_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "skills",
        summary: "Browse available skills",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "plugins",
        summary: "Browse available plugins",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "agents",
        summary: "Browse available agent definitions",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "themes",
        summary: "Browse available TUI themes",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "search",
        summary: "Search the marketplace",
        args: &[ArgSpec::FreeText { hint: "<query>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "install",
        summary: "Install a package by name",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "info",
        summary: "Show details for a package",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
];

pub const PROVIDER_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "list",
        summary: "List all configured providers",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "add",
        summary: "Add a provider with an API key",
        args: &[
            ArgSpec::DynamicEnum(DynamicEnumSource::Providers),
            ArgSpec::FreeText { hint: "<api-key>" },
        ],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "remove",
        summary: "Remove a provider",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::Providers)],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "anthropic",
        summary: "Switch to Anthropic (Claude)",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "openai",
        summary: "Switch to OpenAI",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "ollama",
        summary: "Switch to local Ollama",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "xai",
        summary: "Switch to xAI (Grok)",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "login",
        summary: "Authenticate the current provider",
        args: &[],
        subcommands: &[],
    },
];

pub const LOGIN_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "anthropic",
        summary: "Login to Anthropic",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "openai",
        summary: "Login to OpenAI",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "ollama",
        summary: "Login to Ollama",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "xai",
        summary: "Login to xAI",
        args: &[],
        subcommands: &[],
    },
];

pub const FAILOVER_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "add",
        summary: "Add a model to the failover chain",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::Models)],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "remove",
        summary: "Remove entry N from the failover chain",
        args: &[ArgSpec::FreeText { hint: "<n>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "clear",
        summary: "Clear the entire failover chain",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "list",
        summary: "List the failover chain",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "test",
        summary: "Test a model endpoint",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::Models)],
        subcommands: &[],
    },
];

pub const LANGUAGE_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "en", summary: "English", args: &[], subcommands: &[] },
    SubcommandSpec { name: "de", summary: "German", args: &[], subcommands: &[] },
    SubcommandSpec { name: "es", summary: "Spanish", args: &[], subcommands: &[] },
    SubcommandSpec { name: "fr", summary: "French", args: &[], subcommands: &[] },
    SubcommandSpec { name: "ja", summary: "Japanese", args: &[], subcommands: &[] },
    SubcommandSpec { name: "zh-CN", summary: "Simplified Chinese", args: &[], subcommands: &[] },
    SubcommandSpec { name: "ru", summary: "Russian", args: &[], subcommands: &[] },
];

pub const THEME_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "list",
        summary: "List available themes",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "set",
        summary: "Switch to a theme",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::InstalledThemes)],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "reset",
        summary: "Reset to the default theme",
        args: &[],
        subcommands: &[],
    },
];

pub const AGENTS_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "list",
        summary: "List installed agents",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "info",
        summary: "Show details for an agent",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::InstalledAgents)],
        subcommands: &[],
    },
];

pub const SKILLS_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "list",
        summary: "List available skills",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "info",
        summary: "Show details for a skill",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::InstalledSkills)],
        subcommands: &[],
    },
];

pub const BRANCH_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "create",
        summary: "Create a new branch",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "delete",
        summary: "Delete a branch",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "switch",
        summary: "Switch to a branch",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "list",
        summary: "List all branches",
        args: &[],
        subcommands: &[],
    },
];

pub const WORKTREE_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "create",
        summary: "Create a new worktree",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "list",
        summary: "List all worktrees",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "remove",
        summary: "Remove a worktree",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
];

pub const GIT_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "status", summary: "Show working tree status", args: &[], subcommands: &[] },
    SubcommandSpec { name: "log", summary: "Show commit log", args: &[], subcommands: &[] },
    SubcommandSpec { name: "diff", summary: "Show unstaged diff", args: &[], subcommands: &[] },
    SubcommandSpec { name: "rebase", summary: "Interactive rebase helper", args: &[], subcommands: &[] },
    SubcommandSpec { name: "stash", summary: "Stash uncommitted changes", args: &[], subcommands: &[] },
    SubcommandSpec {
        name: "cherry-pick",
        summary: "Cherry-pick a commit",
        args: &[ArgSpec::FreeText { hint: "<sha>" }],
        subcommands: &[],
    },
];

pub const DB_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "connect",
        summary: "Connect to a database",
        args: &[ArgSpec::FreeText { hint: "<url>" }],
        subcommands: &[],
    },
    SubcommandSpec { name: "schema", summary: "Inspect the database schema", args: &[], subcommands: &[] },
    SubcommandSpec {
        name: "query",
        summary: "Run a SQL query",
        args: &[ArgSpec::FreeText { hint: "<sql>" }],
        subcommands: &[],
    },
    SubcommandSpec { name: "migrate", summary: "Apply pending migrations", args: &[], subcommands: &[] },
];

pub const DOCKER_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "ps", summary: "List running containers", args: &[], subcommands: &[] },
    SubcommandSpec {
        name: "logs",
        summary: "Tail container logs",
        args: &[ArgSpec::FreeText { hint: "<container>" }],
        subcommands: &[],
    },
    SubcommandSpec { name: "compose", summary: "Show docker-compose services", args: &[], subcommands: &[] },
    SubcommandSpec { name: "build", summary: "Build compose images", args: &[], subcommands: &[] },
    SubcommandSpec {
        name: "run",
        summary: "Run a container image",
        args: &[ArgSpec::FreeText { hint: "<image>" }],
        subcommands: &[],
    },
];

pub const TEST_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "generate",
        summary: "AI-generate tests for a source file",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
    SubcommandSpec { name: "run", summary: "Run the test suite", args: &[], subcommands: &[] },
    SubcommandSpec { name: "coverage", summary: "Show coverage report", args: &[], subcommands: &[] },
];

pub const REFACTOR_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "rename",
        summary: "Rename a symbol",
        args: &[
            ArgSpec::FreeText { hint: "<old>" },
            ArgSpec::FreeText { hint: "<new>" },
        ],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "extract",
        summary: "Extract lines into a function",
        args: &[
            ArgSpec::FreeText { hint: "<file>" },
            ArgSpec::FreeText { hint: "<lines>" },
        ],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "move",
        summary: "Move code from src to dst",
        args: &[
            ArgSpec::FreeText { hint: "<src>" },
            ArgSpec::FreeText { hint: "<dst>" },
        ],
        subcommands: &[],
    },
];

pub const SECURITY_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "scan", summary: "Static analysis vulnerability scan", args: &[], subcommands: &[] },
    SubcommandSpec { name: "secrets", summary: "Search for accidentally committed secrets", args: &[], subcommands: &[] },
    SubcommandSpec { name: "deps", summary: "Audit dependencies for CVEs", args: &[], subcommands: &[] },
    SubcommandSpec { name: "report", summary: "Generate a full security report", args: &[], subcommands: &[] },
];

pub const API_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "spec",
        summary: "Parse and display an API spec",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "mock",
        summary: "Start a mock server from an API spec",
        args: &[ArgSpec::FreeText { hint: "<spec>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "test",
        summary: "Test an API endpoint",
        args: &[ArgSpec::FreeText { hint: "<endpoint>" }],
        subcommands: &[],
    },
    SubcommandSpec { name: "docs", summary: "Generate API documentation", args: &[], subcommands: &[] },
];

pub const DOCS_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "generate", summary: "Generate inline documentation", args: &[], subcommands: &[] },
    SubcommandSpec { name: "readme", summary: "Generate or update README.md", args: &[], subcommands: &[] },
    SubcommandSpec { name: "architecture", summary: "Generate architecture diagram", args: &[], subcommands: &[] },
    SubcommandSpec { name: "changelog", summary: "Generate CHANGELOG entry", args: &[], subcommands: &[] },
];

pub const SCAFFOLD_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "new",
        summary: "Scaffold a new project from a template",
        args: &[ArgSpec::FreeText { hint: "<template>" }],
        subcommands: &[],
    },
    SubcommandSpec { name: "list", summary: "List available templates", args: &[], subcommands: &[] },
];

pub const PERF_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "profile",
        summary: "Profile a command",
        args: &[ArgSpec::FreeText { hint: "<cmd>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "benchmark",
        summary: "Benchmark a file",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
    SubcommandSpec { name: "flamegraph", summary: "Generate a flamegraph", args: &[], subcommands: &[] },
    SubcommandSpec { name: "analyze", summary: "Analyze performance data", args: &[], subcommands: &[] },
];

pub const DEBUG_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "start",
        summary: "Start debugger for a file",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "breakpoint",
        summary: "Set a breakpoint at file:line",
        args: &[ArgSpec::FreeText { hint: "<file:line>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "watch",
        summary: "Watch an expression",
        args: &[ArgSpec::FreeText { hint: "<expr>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "explain",
        summary: "AI-explain an error message",
        args: &[ArgSpec::FreeText { hint: "<error>" }],
        subcommands: &[],
    },
];

pub const VOICE_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "start", summary: "Start microphone capture", args: &[], subcommands: &[] },
    SubcommandSpec { name: "stop", summary: "Stop microphone capture", args: &[], subcommands: &[] },
];

pub const COLLAB_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "share", summary: "Share this session", args: &[], subcommands: &[] },
    SubcommandSpec {
        name: "join",
        summary: "Join a shared session by ID",
        args: &[ArgSpec::FreeText { hint: "<id>" }],
        subcommands: &[],
    },
];

pub const ENV_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "show", summary: "Show session environment variables", args: &[], subcommands: &[] },
    SubcommandSpec {
        name: "set",
        summary: "Set an environment variable",
        args: &[
            ArgSpec::FreeText { hint: "<key>" },
            ArgSpec::FreeText { hint: "<value>" },
        ],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "load",
        summary: "Load variables from a .env file",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
    SubcommandSpec { name: "diff", summary: "Diff two environments", args: &[], subcommands: &[] },
];

pub const LSP_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "start",
        summary: "Start a language server",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::Languages)],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "symbols",
        summary: "List symbols in a file",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "references",
        summary: "Find references to a symbol",
        args: &[ArgSpec::FreeText { hint: "<symbol>" }],
        subcommands: &[],
    },
];

pub const NOTEBOOK_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "run",
        summary: "Execute a notebook file",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "cell",
        summary: "Run a specific cell",
        args: &[
            ArgSpec::FreeText { hint: "<file>" },
            ArgSpec::FreeText { hint: "<n>" },
        ],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "export",
        summary: "Export notebook to a format",
        args: &[
            ArgSpec::FreeText { hint: "<file>" },
            ArgSpec::FreeText { hint: "<format>" },
        ],
        subcommands: &[],
    },
];

pub const K8S_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "pods", summary: "List all pods", args: &[], subcommands: &[] },
    SubcommandSpec {
        name: "logs",
        summary: "Stream logs from a pod",
        args: &[ArgSpec::FreeText { hint: "<pod>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "apply",
        summary: "Apply a manifest file",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "describe",
        summary: "Describe a resource",
        args: &[ArgSpec::FreeText { hint: "<resource>" }],
        subcommands: &[],
    },
];

pub const IAC_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "plan", summary: "Show Terraform plan", args: &[], subcommands: &[] },
    SubcommandSpec { name: "apply", summary: "Apply Terraform changes", args: &[], subcommands: &[] },
    SubcommandSpec { name: "validate", summary: "Validate Terraform config", args: &[], subcommands: &[] },
    SubcommandSpec { name: "drift", summary: "Detect infrastructure drift", args: &[], subcommands: &[] },
];

pub const PIPELINE_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "generate", summary: "Generate a CI/CD pipeline", args: &[], subcommands: &[] },
    SubcommandSpec { name: "lint", summary: "Lint the pipeline config", args: &[], subcommands: &[] },
    SubcommandSpec { name: "run", summary: "Run the pipeline locally", args: &[], subcommands: &[] },
];

pub const REVIEW_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "staged",
        summary: "Review all staged git changes",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec { name: "pr", summary: "Review the current PR diff", args: &[], subcommands: &[] },
];

pub const DEPS_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "tree", summary: "Show dependency tree", args: &[], subcommands: &[] },
    SubcommandSpec { name: "outdated", summary: "List outdated dependencies", args: &[], subcommands: &[] },
    SubcommandSpec { name: "audit", summary: "Audit for known CVEs", args: &[], subcommands: &[] },
    SubcommandSpec {
        name: "why",
        summary: "Explain why a package is included",
        args: &[ArgSpec::FreeText { hint: "<pkg>" }],
        subcommands: &[],
    },
];

pub const MONO_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "list", summary: "List workspace packages", args: &[], subcommands: &[] },
    SubcommandSpec { name: "graph", summary: "Show dependency graph", args: &[], subcommands: &[] },
    SubcommandSpec { name: "changed", summary: "List packages changed since main", args: &[], subcommands: &[] },
    SubcommandSpec {
        name: "run",
        summary: "Run a command across workspace packages",
        args: &[
            ArgSpec::FreeText { hint: "<cmd>" },
            ArgSpec::OptionalFlag {
                name: "--filter",
                value: Some(ArgSpecValue::FreeText { hint: "<pkg>" }),
            },
        ],
        subcommands: &[],
    },
];

pub const BROWSER_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "open",
        summary: "Open a URL in the browser",
        args: &[ArgSpec::FreeText { hint: "<url>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "screenshot",
        summary: "Capture a screenshot of a URL",
        args: &[ArgSpec::FreeText { hint: "<url>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "test",
        summary: "Run accessibility/smoke test on a URL",
        args: &[ArgSpec::FreeText { hint: "<url>" }],
        subcommands: &[],
    },
];

pub const NOTIFY_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "send",
        summary: "Send a desktop notification",
        args: &[ArgSpec::FreeText { hint: "<msg>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "webhook",
        summary: "POST a message to a webhook URL",
        args: &[
            ArgSpec::FreeText { hint: "<url>" },
            ArgSpec::FreeText { hint: "<msg>" },
        ],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "matrix",
        summary: "Send to a Matrix room",
        args: &[
            ArgSpec::FreeText { hint: "<room>" },
            ArgSpec::FreeText { hint: "<msg>" },
        ],
        subcommands: &[],
    },
];

pub const MIGRATE_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "framework",
        summary: "Migrate between frameworks",
        args: &[
            ArgSpec::FreeText { hint: "<from>" },
            ArgSpec::FreeText { hint: "<to>" },
        ],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "language",
        summary: "Migrate between languages",
        args: &[
            ArgSpec::FreeText { hint: "<from>" },
            ArgSpec::FreeText { hint: "<to>" },
        ],
        subcommands: &[],
    },
    SubcommandSpec { name: "deps", summary: "Migrate dependency manager", args: &[], subcommands: &[] },
];

pub const REGEX_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "build",
        summary: "Build a regex from a description",
        args: &[ArgSpec::FreeText { hint: "<description>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "test",
        summary: "Test a regex pattern against input",
        args: &[
            ArgSpec::FreeText { hint: "<pattern>" },
            ArgSpec::FreeText { hint: "<input>" },
        ],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "explain",
        summary: "Explain a regex pattern",
        args: &[ArgSpec::FreeText { hint: "<pattern>" }],
        subcommands: &[],
    },
];

pub const SSH_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "list", summary: "List known SSH hosts", args: &[], subcommands: &[] },
    SubcommandSpec {
        name: "connect",
        summary: "Connect to a host",
        args: &[ArgSpec::FreeText { hint: "<host>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "tunnel",
        summary: "Set up an SSH tunnel",
        args: &[
            ArgSpec::FreeText { hint: "<host>" },
            ArgSpec::FreeText { hint: "<local:remote>" },
        ],
        subcommands: &[],
    },
    SubcommandSpec { name: "keys", summary: "List SSH keys", args: &[], subcommands: &[] },
];

pub const LOGS_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "tail",
        summary: "Tail a log file",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "search",
        summary: "Search a log file for a pattern",
        args: &[
            ArgSpec::FreeText { hint: "<file>" },
            ArgSpec::FreeText { hint: "<pattern>" },
        ],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "analyze",
        summary: "AI-analyze a log file",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "stats",
        summary: "Show log file statistics",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
];

pub const MARKDOWN_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "preview",
        summary: "Preview markdown in TUI",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "toc",
        summary: "Generate table of contents",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "lint",
        summary: "Lint markdown file",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
];

pub const SNIPPETS_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "save",
        summary: "Save a code snippet",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
    SubcommandSpec { name: "list", summary: "List all snippets", args: &[], subcommands: &[] },
    SubcommandSpec {
        name: "get",
        summary: "Retrieve a snippet by name",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "search",
        summary: "Search snippets",
        args: &[ArgSpec::FreeText { hint: "<query>" }],
        subcommands: &[],
    },
];

pub const FINETUNE_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "prepare",
        summary: "Prepare training data file",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "validate",
        summary: "Validate a training data file",
        args: &[ArgSpec::FreeText { hint: "<file>" }],
        subcommands: &[],
    },
    SubcommandSpec { name: "start", summary: "Submit a fine-tuning job", args: &[], subcommands: &[] },
    SubcommandSpec { name: "status", summary: "Check fine-tuning job status", args: &[], subcommands: &[] },
];

pub const WEBHOOK_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "list", summary: "List configured webhooks", args: &[], subcommands: &[] },
    SubcommandSpec {
        name: "add",
        summary: "Add a webhook endpoint",
        args: &[
            ArgSpec::FreeText { hint: "<name>" },
            ArgSpec::FreeText { hint: "<url>" },
        ],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "test",
        summary: "Send a test payload to a webhook",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "remove",
        summary: "Remove a webhook",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
];

pub const PLUGIN_SDK_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "init",
        summary: "Scaffold a new plugin",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
    SubcommandSpec { name: "build", summary: "Build the plugin", args: &[], subcommands: &[] },
    SubcommandSpec { name: "test", summary: "Test the plugin", args: &[], subcommands: &[] },
    SubcommandSpec { name: "publish", summary: "Publish to AnvilHub", args: &[], subcommands: &[] },
];

pub const REMOTE_CONTROL_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "stop", summary: "Stop the web viewer relay", args: &[], subcommands: &[] },
    SubcommandSpec { name: "status", summary: "Show relay status", args: &[], subcommands: &[] },
];

pub const HISTORY_ARCHIVE_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "list", summary: "List archived sessions", args: &[], subcommands: &[] },
    SubcommandSpec {
        name: "search",
        summary: "Search archived sessions",
        args: &[ArgSpec::FreeText { hint: "<q>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "view",
        summary: "View an archived session",
        args: &[ArgSpec::FreeText { hint: "<id>" }],
        subcommands: &[],
    },
];

pub const CONFIGURE_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "providers", summary: "Configure AI providers", args: &[], subcommands: &[] },
    SubcommandSpec { name: "models", summary: "Configure model defaults", args: &[], subcommands: &[] },
    SubcommandSpec { name: "context", summary: "Configure context settings", args: &[], subcommands: &[] },
    SubcommandSpec { name: "search", summary: "Configure search providers", args: &[], subcommands: &[] },
    SubcommandSpec { name: "permissions", summary: "Configure permission mode", args: &[], subcommands: &[] },
    SubcommandSpec { name: "display", summary: "Configure display options", args: &[], subcommands: &[] },
    SubcommandSpec { name: "integrations", summary: "Configure integrations", args: &[], subcommands: &[] },
    SubcommandSpec { name: "language", summary: "Configure display language", args: &[], subcommands: &[] },
    SubcommandSpec { name: "vault", summary: "Configure vault settings", args: &[], subcommands: &[] },
    SubcommandSpec { name: "notifications", summary: "Configure notifications", args: &[], subcommands: &[] },
    SubcommandSpec { name: "failover", summary: "Configure failover chain", args: &[], subcommands: &[] },
    SubcommandSpec { name: "ssh", summary: "Configure SSH settings", args: &[], subcommands: &[] },
    SubcommandSpec { name: "docker-k8s", summary: "Configure Docker & Kubernetes", args: &[], subcommands: &[] },
    SubcommandSpec { name: "database", summary: "Configure database connection", args: &[], subcommands: &[] },
    SubcommandSpec { name: "memory", summary: "Configure memory & archive settings", args: &[], subcommands: &[] },
    SubcommandSpec { name: "plugins-cron", summary: "Configure plugins & cron", args: &[], subcommands: &[] },
    SubcommandSpec { name: "status-line", summary: "Configure status line widgets", args: &[], subcommands: &[] },
    SubcommandSpec { name: "mcp", summary: "Configure MCP servers", args: &[], subcommands: MCP_SUBCOMMANDS },
];

/// Ghost command: /tab — tab management
pub const TAB_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "new", summary: "Open a new tab", args: &[], subcommands: &[] },
    SubcommandSpec { name: "close", summary: "Close the current tab", args: &[], subcommands: &[] },
    SubcommandSpec {
        name: "switch",
        summary: "Switch to tab by index or ID",
        args: &[ArgSpec::FreeText { hint: "<id>" }],
        subcommands: &[],
    },
    SubcommandSpec { name: "list", summary: "List all open tabs", args: &[], subcommands: &[] },
];

/// Ghost command: /share — share current tab as read-only link
pub const SHARE_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec { name: "stop", summary: "Stop the active share", args: &[], subcommands: &[] },
    SubcommandSpec { name: "list", summary: "List active shares", args: &[], subcommands: &[] },
];

/// Output style subcommands for `/output-style`.
pub const OUTPUT_STYLE_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "list",
        summary: "List all available output styles (built-ins + user styles)",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "reset",
        summary: "Reset to the default style (precise)",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "precise",
        summary: "Natural model voice — no extra instructions (default)",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "condensed",
        summary: "Token-economical terse rules, Auto-Clarity active",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "<name>",
        summary: "Activate a user-defined style from ~/.anvil/output-styles/",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::OutputStyles)],
        subcommands: &[],
    },
];

/// Goal tracking subcommands for `/goal`.
pub const GOAL_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "new",
        summary: "Create a new goal and set it active",
        args: &[ArgSpec::FreeText { hint: "\"<description>\"" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "list",
        summary: "List all goals (active, paused, done)",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "resume",
        summary: "Mark a goal active by ID",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::Goals)],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "pause",
        summary: "Pause the active goal (or a specific one by ID)",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::Goals)],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "done",
        summary: "Mark a goal done",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::Goals)],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "show",
        summary: "Show full goal details",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::Goals)],
        subcommands: &[],
    },
];

/// /profile — named profile management (v2.2.11 W4)
pub const PROFILE_SUBCOMMANDS: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "list",
        summary: "List all profiles; marks the active one",
        args: &[],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "use",
        summary: "Switch active profile for this session",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::Profiles)],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "show",
        summary: "Print fields of a profile (or the active one)",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::Profiles)],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "create",
        summary: "Create a new empty profile inheriting current effective config",
        args: &[ArgSpec::FreeText { hint: "<name>" }],
        subcommands: &[],
    },
    SubcommandSpec {
        name: "delete",
        summary: "Remove a named profile",
        args: &[ArgSpec::DynamicEnum(DynamicEnumSource::Profiles)],
        subcommands: &[],
    },
];
