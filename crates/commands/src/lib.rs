// Edition 2024: env::set_var/remove_var require unsafe
#![allow(unsafe_code)]

pub mod agents;
pub mod git;
pub mod handlers;
pub mod plugins;
pub mod skill_chaining;
pub mod skill_triggers;
pub mod specs;
pub mod subcommands;
pub mod traits;

pub use agents::{discover_skill_roots, handle_agents_slash_command, handle_skills_slash_command, load_skill_body, load_skills_from_roots};
pub use skill_chaining::{
    ChainCandidate, ChainEvaluator, ChainEntry, ChainWhen,
    format_chain_candidates, format_chain_hint, render_chains_graph,
};
pub use skill_triggers::{filter_skills, match_triggers, whole_word_match_pub, TriggerMatch};
pub use traits::{
    bundled_catalogue, compose_agent, compose_agent_with_options, format_traits_listing,
    ComposeError, ComposeOptions, ComposedAgent, Trait, TraitCatalogue,
};
pub use git::{
    detect_default_branch, handle_branch_slash_command, handle_commit_push_pr_slash_command,
    handle_commit_slash_command, handle_worktree_slash_command, CommitPushPrRequest,
};
pub use handlers::{handle_slash_command, SlashCommandResult};
pub use plugins::{handle_plugins_slash_command, render_plugins_report, PluginsCommandResult};
pub use specs::{
    render_command_detailed_help, render_slash_command_help, resume_supported_slash_commands,
    slash_command_specs, suggest_completions, suggest_slash_commands, SlashCommandCategory,
    SlashCommandSpec,
};
pub use subcommands::{
    ArgSpec, ArgSpecValue, Completion, CompletionContext, DynamicEnumSource, NoopCompletionContext,
    RestartRequirement, StaticDefaultCompletionContext, SubcommandSpec,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandManifestEntry {
    pub name: String,
    pub source: CommandSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSource {
    Builtin,
    InternalOnly,
    FeatureGated,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandRegistry {
    entries: Vec<CommandManifestEntry>,
}

impl CommandRegistry {
    #[must_use]
    pub const fn new(entries: Vec<CommandManifestEntry>) -> Self {
        Self { entries }
    }

    #[must_use]
    pub fn entries(&self) -> &[CommandManifestEntry] {
        &self.entries
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    /// `/help` — show all commands; `/help <command>` — show detailed help for that command
    Help {
        /// Optional command name to show detailed help for (e.g. `Some("vault")`)
        command: Option<String>,
    },
    Status,
    Compact,
    Branch {
        action: Option<String>,
        target: Option<String>,
    },
    Bughunter {
        scope: Option<String>,
    },
    Worktree {
        action: Option<String>,
        path: Option<String>,
        branch: Option<String>,
    },
    Commit,
    CommitPushPr {
        context: Option<String>,
    },
    Pr {
        context: Option<String>,
    },
    Issue {
        context: Option<String>,
    },
    Ultraplan {
        task: Option<String>,
    },
    Teleport {
        target: Option<String>,
    },
    DebugToolCall,
    Model {
        model: Option<String>,
    },
    Permissions {
        mode: Option<String>,
    },
    Clear {
        confirm: bool,
        /// T4-N: When true, clear EVERY tab in the TUI workspace (not just
        /// the active one). Triggered by `/clear --all` (with `--confirm`).
        all_tabs: bool,
    },
    Cost,
    Resume {
        session_path: Option<String>,
    },
    Config {
        section: Option<String>,
    },
    /// `/memory [show|inspect|promote|forget|why|budget|prune] [arg]`
    /// Inspect and manage all memory tiers (ANVIL.md, vault, nominations, cache, …).
    Memory {
        /// Raw sub-command and optional arg, e.g. `Some("show anvil-md")`.
        action: Option<String>,
    },
    Init,
    Diff,
    Version,
    Export {
        format: Option<String>,
        path: Option<String>,
    },
    Session {
        action: Option<String>,
        target: Option<String>,
    },
    Plugins {
        action: Option<String>,
        target: Option<String>,
    },
    Agents {
        args: Option<String>,
    },
    Skills {
        args: Option<String>,
    },
    Qmd {
        query: Option<String>,
    },
    Undo,
    History {
        show_all: bool,
    },
    Context {
        path: Option<String>,
    },
    Pin {
        path: Option<String>,
    },
    Unpin {
        path: String,
    },
    Chat,
    Vim,
    Web {
        query: String,
    },
    Doctor {
        /// T4-M: optional sub-mode. `Some("release")` runs the release-pipeline
        /// pre-flight self-check (clean tree, RELEASE-NOTES file present, tag
        /// vs HEAD, brew shadow, etc). `None` runs the standard runtime check.
        mode: Option<String>,
    },
    Tokens,
    Provider {
        action: Option<String>,
    },
    Login {
        provider: Option<String>,
    },
    Search {
        args: Option<String>,
    },
    Failover {
        action: Option<String>,
    },
    GenerateImage {
        prompt: String,
        wp_post_id: Option<String>,
    },
    HistoryArchive {
        /// The sub-command: `None` = list, `Some("search <q>")`, `Some("view <id>")`.
        action: Option<String>,
    },
    Configure {
        /// Optional section and arguments, e.g. `Some("providers")`, `Some("models default claude-sonnet-4-6")`.
        args: Option<String>,
    },
    /// `/theme`, `/theme list`, `/theme set <name>`, `/theme reset`
    Theme {
        action: Option<String>,
    },
    /// `/semantic-search <query> …`
    SemanticSearch {
        args: Option<String>,
    },
    /// `/docker [ps|logs <container>|compose|build]`
    Docker {
        action: Option<String>,
    },
    /// `/test [generate <file>|run|coverage]`
    Test {
        action: Option<String>,
    },
    /// `/git [rebase|conflicts|cherry-pick <sha>|stash …]`
    Git {
        action: Option<String>,
    },
    /// `/refactor [rename <old> <new>|extract <file> <lines>|move <src> <dst>]`
    Refactor {
        action: Option<String>,
    },
    /// `/screenshot` — capture screen and send to AI via vision
    Screenshot,
    /// `/db [connect <url>|schema|query <sql>|migrate]`
    Db {
        action: Option<String>,
    },
    /// `/security [scan|secrets|deps|report]`
    Security {
        action: Option<String>,
    },
    /// `/api [spec <file>|mock <spec>|test <endpoint>|docs]`
    Api {
        action: Option<String>,
    },
    /// `/docs [generate|readme|architecture|changelog]`
    Docs {
        action: Option<String>,
    },
    /// `/scaffold [new <template>|list]`
    Scaffold {
        action: Option<String>,
    },
    /// `/perf [profile <command>|benchmark <file>|flamegraph|analyze]`
    Perf {
        action: Option<String>,
    },
    /// `/debug [start <file>|breakpoint <file:line>|watch <expr>|explain <error>]`
    Debug {
        action: Option<String>,
    },
    /// `/voice [start|stop]`
    Voice {
        action: Option<String>,
    },
    /// `/collab [share|join <id>]`
    Collab {
        action: Option<String>,
    },
    /// `/changelog` — generate CHANGELOG.md entry from git log since last tag
    Changelog,
    /// `/env [show|set <key> <value>|load <file>|diff]`
    Env {
        action: Option<String>,
    },
    /// `/hub [search <q>|skills|plugins|agents|themes|install <name>|info <name>]`
    Hub {
        action: Option<String>,
    },
    /// `/language [en|de|es|fr|ja|zh-CN|ru]`
    Language {
        lang: Option<String>,
    },
    /// `/lsp [start <lang>|symbols <file>|references <symbol>]`
    Lsp {
        action: Option<String>,
    },
    /// `/notebook [run <file>|cell <file> <n>|export <file> <format>]`
    Notebook {
        action: Option<String>,
    },
    /// `/k8s [pods|logs <pod>|apply <file>|describe <resource>]`
    K8s {
        action: Option<String>,
    },
    /// `/iac [plan|apply|validate|drift]`
    Iac {
        action: Option<String>,
    },
    /// `/pipeline [generate|lint|run]`
    Pipeline {
        action: Option<String>,
    },
    /// `/review [<file>|staged|pr]`
    Review {
        action: Option<String>,
    },
    /// `/deps [tree|outdated|audit|why <pkg>]`
    Deps {
        action: Option<String>,
    },
    /// `/mono [list|graph|changed|run <cmd> [--filter <pkg>]]`
    Mono {
        action: Option<String>,
    },
    /// `/browser [open <url>|screenshot <url>|test <url>]`
    Browser {
        action: Option<String>,
    },
    /// `/notify [send <msg>|webhook <url> <msg>|matrix <room> <msg>]`
    Notify {
        action: Option<String>,
    },
    /// `/vault [setup|unlock|lock|store <label>|get <label>|list|delete <label>|totp ...]`
    Vault {
        action: Option<String>,
    },
    /// `/migrate [framework <from> <to>|language <from> <to>|deps]`
    Migrate {
        action: Option<String>,
    },
    /// `/regex [build <description>|test <pattern> <input>|explain <pattern>]`
    Regex {
        action: Option<String>,
    },
    /// `/ssh` — open an embedded SSH client tab inside Anvil so the user can
    /// switch between agent chat and live host shells without leaving the TUI.
    /// Three forms:
    ///
    ///   * `/ssh`               → modal form to enter host/port/user/auth/alias
    ///   * `/ssh <alias>`       → look up alias in vault and connect
    ///   * `/ssh save <alias>`  → save the active SSH tab's connection details
    ///                            to the vault under `<alias>` for next time
    ///
    /// `args` carries everything after the command word. (T5-Ssh-A.)
    Ssh {
        args: Option<String>,
    },
    /// `/logs [tail <file>|search <file> <pattern>|analyze <file>|stats <file>]`
    Logs {
        action: Option<String>,
    },
    /// `/markdown [preview <file>|toc <file>|lint <file>]`
    Markdown {
        action: Option<String>,
    },
    /// `/snippets [save <name>|list|get <name>|search <query>]`
    Snippets {
        action: Option<String>,
    },
    /// `/finetune [prepare <file>|validate <file>|start|status]`
    Finetune {
        action: Option<String>,
    },
    /// `/webhook [list|add <name> <url>|test <name>|remove <name>]`
    Webhook {
        action: Option<String>,
    },
    /// `/plugin-sdk [init <name>|build|test|publish]`
    PluginSdk {
        action: Option<String>,
    },
    /// `/sleep` — activate the furnace screensaver immediately
    Sleep,
    /// `/think` — toggle thinking/reasoning mode (for models that support it)
    Think,
    /// `/fast` — toggle fast mode (lower `max_tokens` + concise system prompt prefix)
    Fast,
    /// `/review-pr [<number>]` — fetch a GitHub PR diff and send to AI for review
    ReviewPr {
        /// PR number; if absent, uses the current branch's open PR
        number: Option<String>,
    },
    /// `/remote-control [stop|status]` — start (or stop/query) a web viewer relay session
    RemoteControl {
        /// Sub-command: `None` = start, `Some("stop")` = stop, `Some("status")` = status
        action: Option<String>,
    },
    /// `/loop [prompt]` or `/proactive [prompt]` — autonomous loop mode
    Loop {
        prompt: Option<String>,
    },
    /// `/focus` — toggle focus view (prompt + tool summary + final response only)
    Focus,
    /// `/mcp [list|status|tools <server>]` — MCP server management
    Mcp {
        action: Option<String>,
    },
    /// `/productivity` — show session productivity stats
    Productivity,
    /// `/knowledge [review|accept <N>|reject <N>|list]` — manage knowledge nominations
    Knowledge {
        action: Option<String>,
    },
    /// `/daily [date]` — view daily summary and task reconciliation
    Daily {
        date: Option<String>,
    },
    /// `/tab [new|close|switch <id>|list]` — multi-tab management (v2.2.6)
    Tab {
        action: Option<String>,
    },
    /// `/fork` — duplicate current tab with same conversation context (v2.2.6)
    Fork,
    /// `/share [stop|list]` — share current tab as read-only link (v2.2.6)
    Share {
        action: Option<String>,
    },
    /// `/audit` — composite: /security scan + /deps audit + /vault verify (v2.2.6)
    Audit,
    /// `/restart [--soft]` — restart Anvil (v2.2.6)
    ///
    /// `soft: true`  → reload config without respawning (`/restart --soft`)
    /// `soft: false` → full process respawn after confirmation (`/restart`)
    Restart {
        /// When `true`, reload config in-place; when `false`, fully respawn.
        soft: bool,
    },
    /// `/agent [traits|compose <trait1,trait2,...> "<task>"]` — agentic trait composition
    Agent {
        subcommand: AgentSubcommand,
    },
    /// `/output-style [precise|condensed]` — set output verbosity axis (v2.2.7)
    OutputStyle {
        style: Option<String>,
    },
    /// `/profile [list|use <name>|show [<name>]|create <name>|delete <name>]` — named profiles (v2.2.11 W4)
    Profile {
        /// Sub-command and optional target, e.g. `Some("use work")` or `Some("list")`.
        action: Option<String>,
    },
    /// `/effort [low|medium|high|xhigh]` — get or set the per-session reasoning effort (v2.2.11)
    ///
    /// With no argument: prints the current effort level.
    /// With an argument: sets the session-level override immediately.
    Effort {
        level: Option<String>,
    },
    /// `/skill suggest [<prompt>]` — trigger-matched skill suggestions
    /// `/skill load <name>` — prepend skill body to next system prompt
    /// `/skill list` — alias for /skills
    Skill {
        subcommand: SkillSubcommand,
    },
    /// `/goal [new "<desc>"|list|resume <id>|pause [<id>]|done [<id>]|show [<id>]]`
    /// Goal persistence — track long-running objectives across sessions.
    Goal {
        /// Raw args after `/goal`.
        action: Option<String>,
    },
    FileCache {
        action: Option<String>,
    },
    CmdCache {
        action: Option<String>,
    },
    Unknown(String),
}

/// Sub-commands for `/agent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSubcommand {
    /// `/agent traits` — list all bundled traits
    Traits,
    /// `/agent compose <trait1,trait2,...> "<task>"` — compose a temporary agent
    Compose { traits: Vec<String>, task: String },
}

/// Sub-commands for `/skill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSubcommand {
    /// `/skill suggest [<prompt>]`
    Suggest { prompt: Option<String> },
    /// `/skill load <name>`
    Load { name: String },
    /// `/skill list`
    List,
    /// `/skill chains`
    Chains,
}

impl SlashCommand {
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            return None;
        }

        let mut parts = trimmed.trim_start_matches('/').split_whitespace();
        let command = parts.next().unwrap_or_default();
        Some(match command {
            "help" => Self::Help {
                command: parts.next().map(ToOwned::to_owned),
            },
            "status" => Self::Status,
            "compact" => Self::Compact,
            "branch" => Self::Branch {
                action: parts.next().map(ToOwned::to_owned),
                target: parts.next().map(ToOwned::to_owned),
            },
            "bughunter" => Self::Bughunter {
                scope: remainder_after_command(trimmed, command),
            },
            "worktree" => Self::Worktree {
                action: parts.next().map(ToOwned::to_owned),
                path: parts.next().map(ToOwned::to_owned),
                branch: parts.next().map(ToOwned::to_owned),
            },
            "commit" => Self::Commit,
            "commit-push-pr" => Self::CommitPushPr {
                context: remainder_after_command(trimmed, command),
            },
            "pr" => Self::Pr {
                context: remainder_after_command(trimmed, command),
            },
            "issue" => Self::Issue {
                context: remainder_after_command(trimmed, command),
            },
            "ultraplan" => Self::Ultraplan {
                task: remainder_after_command(trimmed, command),
            },
            "teleport" => Self::Teleport {
                target: remainder_after_command(trimmed, command),
            },
            "debug-tool-call" => Self::DebugToolCall,
            "model" => Self::Model {
                model: parts.next().map(ToOwned::to_owned),
            },
            "permissions" => Self::Permissions {
                mode: parts.next().map(ToOwned::to_owned),
            },
            "clear" => {
                // Accept --confirm and --all in either order.
                let mut confirm = false;
                let mut all_tabs = false;
                for arg in parts.by_ref() {
                    match arg {
                        "--confirm" => confirm = true,
                        "--all" => all_tabs = true,
                        _ => {}
                    }
                }
                Self::Clear { confirm, all_tabs }
            },
            "cost" | "usage" | "stats" => Self::Cost,
            "resume" => Self::Resume {
                session_path: parts.next().map(ToOwned::to_owned),
            },
            "config" => Self::Config {
                section: parts.next().map(ToOwned::to_owned),
            },
            "memory" => Self::Memory {
                action: remainder_after_command(trimmed, command),
            },
            "init" => Self::Init,
            "diff" => Self::Diff,
            "version" => Self::Version,
            "export" => {
                let first = parts.next().map(ToOwned::to_owned);
                let (format, path) = match first.as_deref() {
                    Some("md" | "markdown") => (Some("md".to_string()), parts.next().map(ToOwned::to_owned)),
                    Some("text" | "txt") => (Some("text".to_string()), parts.next().map(ToOwned::to_owned)),
                    _ => (None, first),
                };
                Self::Export { format, path }
            }
            "session" => Self::Session {
                action: parts.next().map(ToOwned::to_owned),
                target: parts.next().map(ToOwned::to_owned),
            },
            "plugin" | "plugins" | "marketplace" => Self::Plugins {
                action: parts.next().map(ToOwned::to_owned),
                target: {
                    let remainder = parts.collect::<Vec<_>>().join(" ");
                    (!remainder.is_empty()).then_some(remainder)
                },
            },
            "agents" => Self::Agents {
                args: remainder_after_command(trimmed, command),
            },
            "skills" => Self::Skills {
                args: remainder_after_command(trimmed, command),
            },
            "qmd" => Self::Qmd {
                query: remainder_after_command(trimmed, command),
            },
            "undo" => Self::Undo,
            "history" => Self::History {
                show_all: parts.next() == Some("all"),
            },
            "context" => Self::Context {
                path: remainder_after_command(trimmed, command),
            },
            "pin" => Self::Pin {
                path: remainder_after_command(trimmed, command),
            },
            "unpin" => {
                let path = remainder_after_command(trimmed, command).unwrap_or_default();
                Self::Unpin { path }
            }
            "chat" => Self::Chat,
            "vim" => Self::Vim,
            "web" => {
                let query = remainder_after_command(trimmed, command).unwrap_or_default();
                Self::Web { query }
            }
            "doctor" => Self::Doctor {
                mode: parts.next().map(ToOwned::to_owned),
            },
            "tokens" => Self::Tokens,
            "provider" | "providers" => {
                Self::Provider {
                    action: remainder_after_command(trimmed, command).filter(|s| !s.is_empty()),
                }
            }
            "login" => {
                Self::Login {
                    provider: remainder_after_command(trimmed, command).filter(|s| !s.is_empty()),
                }
            }
            "search" => Self::Search {
                args: remainder_after_command(trimmed, command),
            },
            "failover" => Self::Failover {
                action: remainder_after_command(trimmed, command).filter(|s| !s.is_empty()),
            },
            "generate-image" | "image" => {
                let remainder = remainder_after_command(trimmed, command).unwrap_or_default();
                // Check for optional --wp <post-id> flag at the start
                let (wp_post_id, prompt) = if let Some(rest) = remainder.strip_prefix("--wp ") {
                    let mut iter = rest.splitn(2, ' ');
                    let post_id = iter.next().map(ToOwned::to_owned);
                    let prompt_text = iter.next().unwrap_or("").trim().to_string();
                    (post_id, prompt_text)
                } else {
                    (None, remainder)
                };
                Self::GenerateImage { prompt, wp_post_id }
            }
            "history-archive" => Self::HistoryArchive {
                action: remainder_after_command(trimmed, command),
            },
            "configure" | "settings" | "config-menu" => Self::Configure {
                args: remainder_after_command(trimmed, command),
            },
            "theme" => Self::Theme {
                action: remainder_after_command(trimmed, command),
            },
            "semantic-search" | "symsearch" => Self::SemanticSearch {
                args: remainder_after_command(trimmed, command),
            },
            "docker" => Self::Docker {
                action: remainder_after_command(trimmed, command),
            },
            "test" => Self::Test {
                action: remainder_after_command(trimmed, command),
            },
            "git" => Self::Git {
                action: remainder_after_command(trimmed, command),
            },
            "refactor" => Self::Refactor {
                action: remainder_after_command(trimmed, command),
            },
            "screenshot" => Self::Screenshot,
            "db" => Self::Db {
                action: remainder_after_command(trimmed, command),
            },
            "security" => Self::Security {
                action: remainder_after_command(trimmed, command),
            },
            "api" => Self::Api {
                action: remainder_after_command(trimmed, command),
            },
            "docs" => Self::Docs {
                action: remainder_after_command(trimmed, command),
            },
            "scaffold" => Self::Scaffold {
                action: remainder_after_command(trimmed, command),
            },
            "perf" => Self::Perf {
                action: remainder_after_command(trimmed, command),
            },
            "debug" => Self::Debug {
                action: remainder_after_command(trimmed, command),
            },
            "voice" => Self::Voice {
                action: remainder_after_command(trimmed, command),
            },
            "collab" => Self::Collab {
                action: remainder_after_command(trimmed, command),
            },
            "changelog" => Self::Changelog,
            "env" => Self::Env {
                action: remainder_after_command(trimmed, command),
            },
            "hub" => Self::Hub {
                action: remainder_after_command(trimmed, command),
            },
            "language" | "lang" => Self::Language {
                lang: remainder_after_command(trimmed, command).filter(|s| !s.is_empty()),
            },
            "lsp" => Self::Lsp {
                action: remainder_after_command(trimmed, command),
            },
            "notebook" => Self::Notebook {
                action: remainder_after_command(trimmed, command),
            },
            "k8s" | "kubectl" => Self::K8s {
                action: remainder_after_command(trimmed, command),
            },
            "iac" | "terraform" => Self::Iac {
                action: remainder_after_command(trimmed, command),
            },
            "pipeline" => Self::Pipeline {
                action: remainder_after_command(trimmed, command),
            },
            "review" => Self::Review {
                action: remainder_after_command(trimmed, command),
            },
            "deps" => Self::Deps {
                action: remainder_after_command(trimmed, command),
            },
            "mono" => Self::Mono {
                action: remainder_after_command(trimmed, command),
            },
            "browser" => Self::Browser {
                action: remainder_after_command(trimmed, command),
            },
            "notify" => Self::Notify {
                action: remainder_after_command(trimmed, command),
            },
            "vault" => Self::Vault {
                action: remainder_after_command(trimmed, command),
            },
            "migrate" => Self::Migrate {
                action: remainder_after_command(trimmed, command),
            },
            "regex" => Self::Regex {
                action: remainder_after_command(trimmed, command),
            },
            "logs" => Self::Logs {
                action: remainder_after_command(trimmed, command),
            },
            "markdown" | "md" => Self::Markdown {
                action: remainder_after_command(trimmed, command),
            },
            "snippets" => Self::Snippets {
                action: remainder_after_command(trimmed, command),
            },
            "finetune" => Self::Finetune {
                action: remainder_after_command(trimmed, command),
            },
            "webhook" => Self::Webhook {
                action: remainder_after_command(trimmed, command),
            },
            "ssh" => Self::Ssh {
                args: remainder_after_command(trimmed, command),
            },
            "plugin-sdk" => Self::PluginSdk {
                action: remainder_after_command(trimmed, command),
            },
            "sleep" | "screensaver" | "furnace" => Self::Sleep,
            "think" | "thinking" | "nothink" => Self::Think,
            "fast" => Self::Fast,
            "review-pr" => Self::ReviewPr {
                number: remainder_after_command(trimmed, command).filter(|s| !s.is_empty()),
            },
            "remote-control" | "rc" => Self::RemoteControl {
                action: remainder_after_command(trimmed, command).filter(|s| !s.is_empty()),
            },
            "loop" | "proactive" => Self::Loop {
                prompt: remainder_after_command(trimmed, command).filter(|s| !s.is_empty()),
            },
            "focus" => Self::Focus,
            "mcp" => Self::Mcp {
                action: remainder_after_command(trimmed, command),
            },
            "productivity" => Self::Productivity,
            "knowledge" | "nominations" => Self::Knowledge {
                action: remainder_after_command(trimmed, command),
            },
            "daily" | "summary" => Self::Daily {
                date: remainder_after_command(trimmed, command),
            },
            // ── Ghost commands promoted to real variants (v2.2.6) ─────────────
            "tab" => Self::Tab {
                action: remainder_after_command(trimmed, command),
            },
            "fork" => Self::Fork,
            "share" => Self::Share {
                action: remainder_after_command(trimmed, command),
            },
            "audit" => Self::Audit,
            "restart" => Self::Restart {
                soft: parts.next() == Some("--soft"),
            },
            "agent" => {
                let sub = parts.next().unwrap_or("traits");
                match sub {
                    "traits" | "list" => Self::Agent { subcommand: AgentSubcommand::Traits },
                    "compose" => {
                        // /agent compose security,skeptical "audit auth.rs"
                        let trait_str = parts.next().unwrap_or_default();
                        let traits: Vec<String> = trait_str
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        let task_parts: Vec<&str> = parts.collect();
                        let task = task_parts.join(" ")
                            .trim_matches('"')
                            .trim()
                            .to_string();
                        Self::Agent { subcommand: AgentSubcommand::Compose { traits, task } }
                    }
                    _ => Self::Agent { subcommand: AgentSubcommand::Traits },
                }
            }
            "output-style" | "output_style" => Self::OutputStyle {
                style: remainder_after_command(trimmed, command).filter(|s| !s.is_empty()),
            },
            "profile" => Self::Profile {
                action: remainder_after_command(trimmed, command).filter(|s| !s.is_empty()),
            },
            "effort" => Self::Effort {
                level: remainder_after_command(trimmed, command).filter(|s| !s.is_empty()),
            },
            "skill" => {
                let sub = parts.next().unwrap_or("suggest");
                match sub {
                    "chains" => Self::Skill { subcommand: SkillSubcommand::Chains },
                    "list" => Self::Skill { subcommand: SkillSubcommand::List },
                    "load" => {
                        let name = parts.collect::<Vec<_>>().join(" ");
                        Self::Skill { subcommand: SkillSubcommand::Load { name } }
                    }
                    other_sub => {
                        // "suggest" or bare keyword treated as the start of the prompt
                        let first_token = if other_sub != "suggest" {
                            Some(other_sub.to_string())
                        } else {
                            None
                        };
                        let mut prompt_parts: Vec<String> = first_token.into_iter().collect();
                        prompt_parts.extend(parts.map(ToOwned::to_owned));
                        let prompt_str = prompt_parts.join(" ");
                        let prompt_str = prompt_str
                            .trim()
                            .trim_start_matches('"')
                            .trim_end_matches('"')
                            .trim()
                            .to_string();
                        let prompt = if prompt_str.is_empty() { None } else { Some(prompt_str) };
                        Self::Skill { subcommand: SkillSubcommand::Suggest { prompt } }
                    }
                }
            }
            "goal" => Self::Goal {
                action: remainder_after_command(trimmed, command),
            },
            "file-cache" | "fc" => Self::FileCache { action: remainder_after_command(trimmed, command) },
            "cmd-cache" | "cc" => Self::CmdCache { action: remainder_after_command(trimmed, command) },
            other => Self::Unknown(other.to_string()),
        })
    }
}

/// Format a grouped listing of trigger-matched skills for display.
///
/// When `matches` is empty returns the "no matches" fallback.
/// `prompt` is echoed back in the header for context.
#[must_use]
pub fn format_suggestions(matches: &[TriggerMatch], prompt: &str) -> String {
    if matches.is_empty() {
        return format!(
            "No skill suggestions for \"{prompt}\".\n\
             Try a more specific prompt, or /skill list to browse all installed skills."
        );
    }

    let header = format!("Skill suggestions for \"{prompt}\":");
    let mut lines = vec![header];

    let max_name = matches.iter().map(|m| m.skill_name.len()).max().unwrap_or(0);

    for m in matches {
        let padded = format!("{:<width$}", m.skill_name, width = max_name);
        lines.push(format!(
            "  {padded}    matched \"{:<16}\" — /skill load {}",
            m.matched_trigger, m.skill_name
        ));
    }
    lines.push(
        "No matches? Try a more specific prompt, or /skill list to browse all installed skills."
            .to_string(),
    );
    lines.join("\n")
}

/// Build a one-line TUI hint for trigger-matched skills.
/// Returns `None` when `matches` is empty (silent = no hint shown).
#[must_use]
pub fn format_suggestions_hint(matches: &[TriggerMatch]) -> Option<String> {
    if matches.is_empty() {
        return None;
    }
    let parts: Vec<String> = matches
        .iter()
        .map(|m| format!("{} ({})", m.skill_name, m.matched_trigger))
        .collect();
    Some(format!(
        "\u{2726} Skill suggestions: {} \u{2014} /skill load <name> to apply",
        parts.join(" \u{00b7} ")
    ))
}

fn remainder_after_command(input: &str, command: &str) -> Option<String> {
    input
        .trim()
        .strip_prefix(&format!("/{command}"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn normalize_optional_args(args: Option<&str>) -> Option<&str> {
    args.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{
        agents::{
            load_agents_from_roots, load_skills_from_roots, render_agents_report,
            render_skills_report, DefinitionSource, SkillOrigin, SkillRoot,
        },
        git::{
            handle_branch_slash_command, handle_commit_push_pr_slash_command,
            handle_commit_slash_command, handle_worktree_slash_command,
        },
        handlers::handle_slash_command,
        plugins::{handle_plugins_slash_command, render_plugins_report},
        specs::{
            render_slash_command_help, resume_supported_slash_commands, slash_command_specs,
            suggest_slash_commands,
        },
        AgentSubcommand, CommitPushPrRequest, SkillSubcommand, SlashCommand,
    };
    use plugins::{PluginKind, PluginManager, PluginManagerConfig, PluginMetadata, PluginSummary};
    use runtime::{CompactionConfig, ContentBlock, ConversationMessage, MessageRole, Session};
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("commands-plugin-{label}-{nanos}"))
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock")
    }

    fn run_command(cwd: &Path, program: &str, args: &[&str]) -> String {
        let output = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("command should run");
        assert!(
            output.status.success(),
            "{} {} failed: {}",
            program,
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("stdout should be utf8")
    }

    fn init_git_repo(label: &str) -> PathBuf {
        let root = temp_dir(label);
        fs::create_dir_all(&root).expect("repo root");

        let init = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&root)
            .output()
            .expect("git init should run");
        if !init.status.success() {
            let fallback = Command::new("git")
                .arg("init")
                .current_dir(&root)
                .output()
                .expect("fallback git init should run");
            assert!(
                fallback.status.success(),
                "fallback git init should succeed"
            );
            let rename = Command::new("git")
                .args(["branch", "-m", "main"])
                .current_dir(&root)
                .output()
                .expect("git branch -m should run");
            assert!(rename.status.success(), "git branch -m main should succeed");
        }

        run_command(&root, "git", &["config", "user.name", "Anvil Tests"]);
        run_command(&root, "git", &["config", "user.email", "anvil@example.com"]);
        fs::write(root.join("README.md"), "seed\n").expect("seed file");
        run_command(&root, "git", &["add", "README.md"]);
        run_command(&root, "git", &["commit", "-m", "chore: seed repo"]);
        root
    }

    fn init_bare_repo(label: &str) -> PathBuf {
        let root = temp_dir(label);
        let output = Command::new("git")
            .args(["init", "--bare"])
            .arg(&root)
            .output()
            .expect("bare repo should initialize");
        assert!(output.status.success(), "git init --bare should succeed");
        root
    }

    #[cfg(unix)]
    fn write_fake_gh(bin_dir: &Path, log_path: &Path, url: &str) {
        fs::create_dir_all(bin_dir).expect("bin dir");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo 'gh 1.0.0'\n  exit 0\nfi\nprintf '%s\\n' \"$*\" >> \"{}\"\nif [ \"$1\" = \"pr\" ] && [ \"$2\" = \"create\" ]; then\n  echo '{}'\n  exit 0\nfi\nif [ \"$1\" = \"pr\" ] && [ \"$2\" = \"view\" ]; then\n  echo '{{\"url\":\"{}\"}}'\n  exit 0\nfi\nexit 0\n",
            log_path.display(),
            url,
            url,
        );
        let path = bin_dir.join("gh");
        fs::write(&path, script).expect("gh stub");
        let mut permissions = fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod");
    }

    fn write_external_plugin(root: &Path, name: &str, version: &str) {
        fs::create_dir_all(root.join(".anvil-plugin")).expect("manifest dir");
        fs::write(
            root.join(".anvil-plugin").join("plugin.json"),
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"description\": \"commands plugin\"\n}}"
            ),
        )
        .expect("write manifest");
    }

    fn write_bundled_plugin(root: &Path, name: &str, version: &str, default_enabled: bool) {
        fs::create_dir_all(root.join(".anvil-plugin")).expect("manifest dir");
        fs::write(
            root.join(".anvil-plugin").join("plugin.json"),
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"description\": \"bundled commands plugin\",\n  \"defaultEnabled\": {}\n}}",
                if default_enabled { "true" } else { "false" }
            ),
        )
        .expect("write bundled manifest");
    }

    fn write_agent(root: &Path, name: &str, description: &str, model: &str, reasoning: &str) {
        fs::create_dir_all(root).expect("agent root");
        fs::write(
            root.join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\ndescription = \"{description}\"\nmodel = \"{model}\"\nmodel_reasoning_effort = \"{reasoning}\"\n"
            ),
        )
        .expect("write agent");
    }

    fn write_skill(root: &Path, name: &str, description: &str) {
        let skill_root = root.join(name);
        fs::create_dir_all(&skill_root).expect("skill root");
        fs::write(
            skill_root.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n"),
        )
        .expect("write skill");
    }

    fn write_legacy_command(root: &Path, name: &str, description: &str) {
        fs::create_dir_all(root).expect("commands root");
        fs::write(
            root.join(format!("{name}.md")),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n"),
        )
        .expect("write command");
    }

    #[allow(clippy::too_many_lines)]
    #[test]
    fn parses_supported_slash_commands() {
        assert_eq!(
            SlashCommand::parse("/help"),
            Some(SlashCommand::Help { command: None })
        );
        assert_eq!(
            SlashCommand::parse("/help vault"),
            Some(SlashCommand::Help { command: Some("vault".to_string()) })
        );
        assert_eq!(SlashCommand::parse(" /status "), Some(SlashCommand::Status));
        assert_eq!(
            SlashCommand::parse("/bughunter runtime"),
            Some(SlashCommand::Bughunter {
                scope: Some("runtime".to_string())
            })
        );
        assert_eq!(
            SlashCommand::parse("/branch create feature/demo"),
            Some(SlashCommand::Branch {
                action: Some("create".to_string()),
                target: Some("feature/demo".to_string()),
            })
        );
        assert_eq!(
            SlashCommand::parse("/worktree add ../demo wt-demo"),
            Some(SlashCommand::Worktree {
                action: Some("add".to_string()),
                path: Some("../demo".to_string()),
                branch: Some("wt-demo".to_string()),
            })
        );
        assert_eq!(SlashCommand::parse("/commit"), Some(SlashCommand::Commit));
        assert_eq!(
            SlashCommand::parse("/commit-push-pr ready for review"),
            Some(SlashCommand::CommitPushPr {
                context: Some("ready for review".to_string())
            })
        );
        assert_eq!(
            SlashCommand::parse("/pr ready for review"),
            Some(SlashCommand::Pr {
                context: Some("ready for review".to_string())
            })
        );
        assert_eq!(
            SlashCommand::parse("/issue flaky test"),
            Some(SlashCommand::Issue {
                context: Some("flaky test".to_string())
            })
        );
        assert_eq!(
            SlashCommand::parse("/ultraplan ship both features"),
            Some(SlashCommand::Ultraplan {
                task: Some("ship both features".to_string())
            })
        );
        assert_eq!(
            SlashCommand::parse("/teleport conversation.rs"),
            Some(SlashCommand::Teleport {
                target: Some("conversation.rs".to_string())
            })
        );
        assert_eq!(
            SlashCommand::parse("/debug-tool-call"),
            Some(SlashCommand::DebugToolCall)
        );
        assert_eq!(
            SlashCommand::parse("/model opus"),
            Some(SlashCommand::Model {
                model: Some("opus".to_string()),
            })
        );
        assert_eq!(
            SlashCommand::parse("/model"),
            Some(SlashCommand::Model { model: None })
        );
        assert_eq!(
            SlashCommand::parse("/permissions read-only"),
            Some(SlashCommand::Permissions {
                mode: Some("read-only".to_string()),
            })
        );
        assert_eq!(
            SlashCommand::parse("/clear"),
            Some(SlashCommand::Clear { confirm: false, all_tabs: false })
        );
        assert_eq!(
            SlashCommand::parse("/clear --confirm"),
            Some(SlashCommand::Clear { confirm: true, all_tabs: false })
        );
        assert_eq!(
            SlashCommand::parse("/clear --all --confirm"),
            Some(SlashCommand::Clear { confirm: true, all_tabs: true })
        );
        assert_eq!(
            SlashCommand::parse("/clear --confirm --all"),
            Some(SlashCommand::Clear { confirm: true, all_tabs: true })
        );
        assert_eq!(SlashCommand::parse("/cost"), Some(SlashCommand::Cost));
        assert_eq!(SlashCommand::parse("/usage"), Some(SlashCommand::Cost));
        assert_eq!(SlashCommand::parse("/stats"), Some(SlashCommand::Cost));
        assert_eq!(
            SlashCommand::parse("/resume session.json"),
            Some(SlashCommand::Resume {
                session_path: Some("session.json".to_string()),
            })
        );
        assert_eq!(
            SlashCommand::parse("/config"),
            Some(SlashCommand::Config { section: None })
        );
        assert_eq!(
            SlashCommand::parse("/config env"),
            Some(SlashCommand::Config {
                section: Some("env".to_string())
            })
        );
        assert_eq!(
            SlashCommand::parse("/memory"),
            Some(SlashCommand::Memory { action: None })
        );
        assert_eq!(
            SlashCommand::parse("/memory show anvil-md"),
            Some(SlashCommand::Memory {
                action: Some("show anvil-md".to_string())
            })
        );
        assert_eq!(SlashCommand::parse("/init"), Some(SlashCommand::Init));
        assert_eq!(SlashCommand::parse("/diff"), Some(SlashCommand::Diff));
        assert_eq!(SlashCommand::parse("/version"), Some(SlashCommand::Version));
        assert_eq!(
            SlashCommand::parse("/export notes.txt"),
            Some(SlashCommand::Export {
                format: None,
                path: Some("notes.txt".to_string())
            })
        );
        assert_eq!(
            SlashCommand::parse("/session switch abc123"),
            Some(SlashCommand::Session {
                action: Some("switch".to_string()),
                target: Some("abc123".to_string())
            })
        );
        assert_eq!(
            SlashCommand::parse("/plugins install demo"),
            Some(SlashCommand::Plugins {
                action: Some("install".to_string()),
                target: Some("demo".to_string())
            })
        );
        assert_eq!(
            SlashCommand::parse("/plugins list"),
            Some(SlashCommand::Plugins {
                action: Some("list".to_string()),
                target: None
            })
        );
        assert_eq!(
            SlashCommand::parse("/plugins enable demo"),
            Some(SlashCommand::Plugins {
                action: Some("enable".to_string()),
                target: Some("demo".to_string())
            })
        );
        assert_eq!(
            SlashCommand::parse("/plugins disable demo"),
            Some(SlashCommand::Plugins {
                action: Some("disable".to_string()),
                target: Some("demo".to_string())
            })
        );
    }

    #[test]
    fn parses_effort_slash_command() {
        assert_eq!(
            SlashCommand::parse("/effort"),
            Some(SlashCommand::Effort { level: None })
        );
        assert_eq!(
            SlashCommand::parse("/effort low"),
            Some(SlashCommand::Effort { level: Some("low".to_string()) })
        );
        assert_eq!(
            SlashCommand::parse("/effort medium"),
            Some(SlashCommand::Effort { level: Some("medium".to_string()) })
        );
        assert_eq!(
            SlashCommand::parse("/effort high"),
            Some(SlashCommand::Effort { level: Some("high".to_string()) })
        );
        assert_eq!(
            SlashCommand::parse("/effort xhigh"),
            Some(SlashCommand::Effort { level: Some("xhigh".to_string()) })
        );
    }

    #[test]
    fn renders_help_from_shared_specs() {
        let help = render_slash_command_help();
        assert!(help.contains("available via anvil --resume SESSION.json"));
        assert!(help.contains("Core flow"));
        assert!(help.contains("Workspace & memory"));
        assert!(help.contains("Sessions & output"));
        assert!(help.contains("Git & GitHub"));
        assert!(help.contains("Automation & discovery"));
        assert!(help.contains("/help"));
        assert!(help.contains("/status"));
        assert!(help.contains("/compact"));
        assert!(help.contains("/bughunter [scope]"));
        assert!(help.contains("/branch [list|create <name>|switch <name>]"));
        assert!(help.contains("/worktree [list|add <path> [branch]|remove <path>|prune]"));
        assert!(help.contains("/commit"));
        assert!(help.contains("/commit-push-pr [context]"));
        assert!(help.contains("/pr [context]"));
        assert!(help.contains("/issue [context]"));
        assert!(help.contains("/ultraplan [task]"));
        assert!(help.contains("/teleport <symbol-or-path>"));
        assert!(help.contains("/debug-tool-call"));
        assert!(help.contains("/model [model]"));
        assert!(help.contains("/permissions [read-only|workspace-write|danger-full-access]"));
        assert!(help.contains("/clear [--confirm]"));
        assert!(help.contains("/cost"));
        assert!(help.contains("/resume <session-path>"));
        assert!(help.contains("/config [env|hooks|model|plugins]"));
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
        assert!(help.contains("aliases: /usage, /stats"));
        assert!(help.contains("/agents"));
        assert!(help.contains("/skills"));
        // v2.2.6: added mcp, productivity, knowledge, daily, think, focus, loop,
        //         remote-control (8 previously-missing) + tab, fork, share, audit (4 ghost)
        //         + restart (Phase 5 placeholder) = +13 total
        // v2.2.7+: +3 new commands (agent, output-style, skill) — see spec count audit
        // v2.2.11 W2: +1 (effort), W3: +1 (goal), W4: +1 (profile) = 108 total
        // v2.3 W11: +1 (file-cache), W12: +1 (cmd-cache) = 110 total
        assert_eq!(slash_command_specs().len(), 110);
        // v2.2.6: added knowledge (resume) + daily (resume) + productivity (resume) = +3
        assert_eq!(resume_supported_slash_commands().len(), 24);
    }

    #[test]
    fn suggests_close_slash_commands() {
        // `/stats` is an alias of `/cost` (CC parity FEAT-28), so a near-exact
        // typo for `stats` resolves directly to `/cost`. Use a different typo
        // to exercise the fuzzy suggester for `/status`.
        let suggestions = suggest_slash_commands("statu", 3);
        assert!(!suggestions.is_empty());
        assert_eq!(suggestions[0], "/status");
    }

    #[test]
    fn compacts_sessions_via_slash_command() {
        let session = Session {
            version: 1,
            messages: vec![
                ConversationMessage::user_text("a ".repeat(200)),
                ConversationMessage::assistant(vec![ContentBlock::Text {
                    text: "b ".repeat(200),
                }]),
                ConversationMessage::tool_result("1", "bash", "ok ".repeat(200), false),
                ConversationMessage::assistant(vec![ContentBlock::Text {
                    text: "recent".to_string(),
                }]),
            ],
        };

        let result = handle_slash_command(
            "/compact",
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            },
        )
        .expect("slash command should be handled");

        assert!(result.message.contains("Compacted 2 messages"));
        assert_eq!(result.session.messages[0].role, MessageRole::System);
    }

    #[test]
    fn help_command_is_non_mutating() {
        let session = Session::new();
        let result = handle_slash_command("/help", &session, CompactionConfig::default())
            .expect("help command should be handled");
        assert_eq!(result.session, session);
        assert!(result.message.contains("Slash commands"));
    }

    #[test]
    fn unimplemented_slash_commands_return_helpful_messages() {
        let session = Session::new();

        // Unknown commands return a "not recognized" message
        let unknown =
            handle_slash_command("/unknown", &session, CompactionConfig::default())
                .expect("unknown command should return a message");
        assert!(unknown.message.contains("not a recognized command"));
        assert!(unknown.message.contains("/help"));
        assert_eq!(unknown.session, session);

        // All commands that are parsed but not yet implemented return Some with a
        // helpful message and leave the session unchanged.
        let cases: &[&str] = &[
            "/status",
            "/branch list",
            "/bughunter",
            "/worktree list",
            "/commit",
            "/commit-push-pr review notes",
            "/pr",
            "/issue",
            "/ultraplan",
            "/teleport foo",
            "/debug-tool-call",
            "/model sonnet",
            "/permissions read-only",
            "/clear",
            "/clear --confirm",
            "/cost",
            "/resume session.json",
            "/config",
            "/config env",
            "/diff",
            "/version",
            "/export note.txt",
            "/session list",
            "/plugins list",
        ];

        for input in cases {
            let result = handle_slash_command(input, &session, CompactionConfig::default())
                .unwrap_or_else(|| panic!("{input} should return a message, not None"));
            assert!(
                !result.message.is_empty(),
                "{input} returned an empty message"
            );
            assert_eq!(
                result.session, session,
                "{input} should not mutate the session"
            );
        }
    }

    #[test]
    fn renders_plugins_report_with_name_version_and_status() {
        let rendered = render_plugins_report(&[
            PluginSummary {
                metadata: PluginMetadata {
                    id: "demo@external".to_string(),
                    name: "demo".to_string(),
                    version: "1.2.3".to_string(),
                    description: "demo plugin".to_string(),
                    kind: PluginKind::External,
                    source: "demo".to_string(),
                    default_enabled: false,
                    root: None,
                },
                enabled: true,
            },
            PluginSummary {
                metadata: PluginMetadata {
                    id: "sample@external".to_string(),
                    name: "sample".to_string(),
                    version: "0.9.0".to_string(),
                    description: "sample plugin".to_string(),
                    kind: PluginKind::External,
                    source: "sample".to_string(),
                    default_enabled: false,
                    root: None,
                },
                enabled: false,
            },
        ]);

        assert!(rendered.contains("demo"));
        assert!(rendered.contains("v1.2.3"));
        assert!(rendered.contains("enabled"));
        assert!(rendered.contains("sample"));
        assert!(rendered.contains("v0.9.0"));
        assert!(rendered.contains("disabled"));
    }

    #[test]
    fn lists_agents_from_project_and_user_roots() {
        let workspace = temp_dir("agents-workspace");
        let project_agents = workspace.join(".codex").join("agents");
        let user_home = temp_dir("agents-home");
        let user_agents = user_home.join(".codex").join("agents");

        write_agent(
            &project_agents,
            "planner",
            "Project planner",
            "gpt-5.4",
            "medium",
        );
        write_agent(
            &user_agents,
            "planner",
            "User planner",
            "gpt-5.4-mini",
            "high",
        );
        write_agent(
            &user_agents,
            "verifier",
            "Verification agent",
            "gpt-5.4-mini",
            "high",
        );

        let roots = vec![
            (DefinitionSource::ProjectCodex, project_agents),
            (DefinitionSource::UserCodex, user_agents),
        ];
        let report =
            render_agents_report(&load_agents_from_roots(&roots).expect("agent roots should load"));

        assert!(report.contains("Agents"));
        assert!(report.contains("2 active agents"));
        assert!(report.contains("Project (.codex):"));
        assert!(report.contains("planner · Project planner · gpt-5.4 · medium"));
        assert!(report.contains("User (~/.codex):"));
        assert!(report.contains("(shadowed by Project (.codex)) planner · User planner"));
        assert!(report.contains("verifier · Verification agent · gpt-5.4-mini · high"));

        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(user_home);
    }

    #[test]
    fn lists_skills_from_project_and_user_roots() {
        let workspace = temp_dir("skills-workspace");
        let project_skills = workspace.join(".codex").join("skills");
        let project_commands = workspace.join(".anvil").join("commands");
        let user_home = temp_dir("skills-home");
        let user_skills = user_home.join(".codex").join("skills");

        write_skill(&project_skills, "plan", "Project planning guidance");
        write_legacy_command(&project_commands, "deploy", "Legacy deployment guidance");
        write_skill(&user_skills, "plan", "User planning guidance");
        write_skill(&user_skills, "help", "Help guidance");

        let roots = vec![
            SkillRoot {
                source: DefinitionSource::ProjectCodex,
                path: project_skills,
                origin: SkillOrigin::SkillsDir,
            },
            SkillRoot {
                source: DefinitionSource::ProjectAnvil,
                path: project_commands,
                origin: SkillOrigin::LegacyCommandsDir,
            },
            SkillRoot {
                source: DefinitionSource::UserCodex,
                path: user_skills,
                origin: SkillOrigin::SkillsDir,
            },
        ];
        let report =
            render_skills_report(&load_skills_from_roots(&roots).expect("skill roots should load"));

        assert!(report.contains("Skills"));
        assert!(report.contains("3 available skills"));
        assert!(report.contains("Project (.codex):"));
        assert!(report.contains("plan · Project planning guidance"));
        assert!(report.contains("Project (.anvil):"));
        assert!(report.contains("deploy · Legacy deployment guidance · legacy /commands"));
        assert!(report.contains("User (~/.codex):"));
        assert!(report.contains("(shadowed by Project (.codex)) plan · User planning guidance"));
        assert!(report.contains("help · Help guidance"));

        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(user_home);
    }

    #[test]
    fn agents_and_skills_usage_support_help_and_unexpected_args() {
        let cwd = temp_dir("slash-usage");

        let agents_help = super::agents::handle_agents_slash_command(Some("help"), &cwd)
            .expect("agents help");
        assert!(agents_help.contains("Usage            /agents"));
        assert!(agents_help.contains("Direct CLI       anvil agents"));

        let agents_unexpected =
            super::agents::handle_agents_slash_command(Some("show planner"), &cwd)
                .expect("agents usage");
        assert!(agents_unexpected.contains("Unexpected       show planner"));

        let skills_help = super::agents::handle_skills_slash_command(Some("--help"), &cwd)
            .expect("skills help");
        assert!(skills_help.contains("Usage            /skills"));
        assert!(skills_help.contains("legacy /commands"));

        let skills_unexpected =
            super::agents::handle_skills_slash_command(Some("show help"), &cwd)
                .expect("skills usage");
        assert!(skills_unexpected.contains("Unexpected       show help"));

        let _ = fs::remove_dir_all(cwd);
    }

    #[test]
    fn parses_quoted_skill_frontmatter_values() {
        let contents = "---\nname: \"hud\"\ndescription: 'Quoted description'\n---\n";
        let fm = super::agents::parse_skill_frontmatter(contents);
        assert_eq!(fm.name.as_deref(), Some("hud"));
        assert_eq!(fm.description.as_deref(), Some("Quoted description"));
        assert!(fm.triggers.is_empty());
    }

    #[test]
    fn installs_plugin_from_path_and_lists_it() {
        let config_home = temp_dir("home");
        let source_root = temp_dir("source");
        write_external_plugin(&source_root, "demo", "1.0.0");

        let mut manager = PluginManager::new(PluginManagerConfig::new(&config_home));
        let install = handle_plugins_slash_command(
            Some("install"),
            Some(source_root.to_str().expect("utf8 path")),
            &mut manager,
        )
        .expect("install command should succeed");
        assert!(install.reload_runtime);
        assert!(install.message.contains("installed demo@external"));
        assert!(install.message.contains("Name             demo"));
        assert!(install.message.contains("Version          1.0.0"));
        assert!(install.message.contains("Status           enabled"));

        let list = handle_plugins_slash_command(Some("list"), None, &mut manager)
            .expect("list command should succeed");
        assert!(!list.reload_runtime);
        assert!(list.message.contains("demo"));
        assert!(list.message.contains("v1.0.0"));
        assert!(list.message.contains("enabled"));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn enables_and_disables_plugin_by_name() {
        let config_home = temp_dir("toggle-home");
        let source_root = temp_dir("toggle-source");
        write_external_plugin(&source_root, "demo", "1.0.0");

        let mut manager = PluginManager::new(PluginManagerConfig::new(&config_home));
        handle_plugins_slash_command(
            Some("install"),
            Some(source_root.to_str().expect("utf8 path")),
            &mut manager,
        )
        .expect("install command should succeed");

        let disable = handle_plugins_slash_command(Some("disable"), Some("demo"), &mut manager)
            .expect("disable command should succeed");
        assert!(disable.reload_runtime);
        assert!(disable.message.contains("disabled demo@external"));
        assert!(disable.message.contains("Name             demo"));
        assert!(disable.message.contains("Status           disabled"));

        let list = handle_plugins_slash_command(Some("list"), None, &mut manager)
            .expect("list command should succeed");
        assert!(list.message.contains("demo"));
        assert!(list.message.contains("disabled"));

        let enable = handle_plugins_slash_command(Some("enable"), Some("demo"), &mut manager)
            .expect("enable command should succeed");
        assert!(enable.reload_runtime);
        assert!(enable.message.contains("enabled demo@external"));
        assert!(enable.message.contains("Name             demo"));
        assert!(enable.message.contains("Status           enabled"));

        let list = handle_plugins_slash_command(Some("list"), None, &mut manager)
            .expect("list command should succeed");
        assert!(list.message.contains("demo"));
        assert!(list.message.contains("enabled"));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn lists_auto_installed_bundled_plugins_with_status() {
        let config_home = temp_dir("bundled-home");
        let bundled_root = temp_dir("bundled-root");
        let bundled_plugin = bundled_root.join("starter");
        write_bundled_plugin(&bundled_plugin, "starter", "0.1.0", false);

        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(bundled_root.clone());
        let mut manager = PluginManager::new(config);

        let list = handle_plugins_slash_command(Some("list"), None, &mut manager)
            .expect("list command should succeed");
        assert!(!list.reload_runtime);
        assert!(list.message.contains("starter"));
        assert!(list.message.contains("v0.1.0"));
        assert!(list.message.contains("disabled"));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(bundled_root);
    }

    #[test]
    fn branch_and_worktree_commands_manage_git_state() {
        // given
        let repo = init_git_repo("branch-worktree");
        let worktree_path = repo
            .parent()
            .expect("repo should have parent")
            .join("branch-worktree-linked");

        // when
        let branch_list =
            handle_branch_slash_command(Some("list"), None, &repo).expect("branch list succeeds");
        let created = handle_branch_slash_command(Some("create"), Some("feature/demo"), &repo)
            .expect("branch create succeeds");
        let switched = handle_branch_slash_command(Some("switch"), Some("main"), &repo)
            .expect("branch switch succeeds");
        let added = handle_worktree_slash_command(
            Some("add"),
            Some(worktree_path.to_str().expect("utf8 path")),
            Some("wt-demo"),
            &repo,
        )
        .expect("worktree add succeeds");
        let listed_worktrees =
            handle_worktree_slash_command(Some("list"), None, None, &repo).expect("list succeeds");
        let removed = handle_worktree_slash_command(
            Some("remove"),
            Some(worktree_path.to_str().expect("utf8 path")),
            None,
            &repo,
        )
        .expect("remove succeeds");

        // then
        assert!(branch_list.contains("main"));
        assert!(created.contains("feature/demo"));
        assert!(switched.contains("main"));
        assert!(added.contains("wt-demo"));
        assert!(listed_worktrees.contains(worktree_path.to_str().expect("utf8 path")));
        assert!(removed.contains("Result           removed"));

        let _ = fs::remove_dir_all(repo);
        let _ = fs::remove_dir_all(worktree_path);
    }

    #[test]
    fn commit_command_stages_and_commits_changes() {
        // given
        let repo = init_git_repo("commit-command");
        fs::write(repo.join("notes.txt"), "hello\n").expect("write notes");

        // when
        let report =
            handle_commit_slash_command("feat: add notes", &repo).expect("commit succeeds");
        let status = run_command(&repo, "git", &["status", "--short"]);
        let message = run_command(&repo, "git", &["log", "-1", "--pretty=%B"]);

        // then
        assert!(report.contains("Result           created"));
        assert!(status.trim().is_empty());
        assert_eq!(message.trim(), "feat: add notes");

        let _ = fs::remove_dir_all(repo);
    }

    #[cfg(unix)]
    #[test]
    fn commit_push_pr_command_commits_pushes_and_creates_pr() {
        // given
        let _guard = env_lock();
        let repo = init_git_repo("commit-push-pr");
        let remote = init_bare_repo("commit-push-pr-remote");
        run_command(
            &repo,
            "git",
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("utf8 remote"),
            ],
        );
        run_command(&repo, "git", &["push", "-u", "origin", "main"]);
        fs::write(repo.join("feature.txt"), "feature\n").expect("write feature file");

        let fake_bin = temp_dir("fake-gh-bin");
        let gh_log = fake_bin.join("gh.log");
        write_fake_gh(&fake_bin, &gh_log, "https://example.com/pr/123");

        let previous_path = env::var_os("PATH");
        let new_path = env::join_paths(
            std::iter::once(fake_bin.clone())
                .chain(env::split_paths(&previous_path.clone().unwrap_or_default())),
        )
        .expect("join paths");
        unsafe { env::set_var("PATH", &new_path); }
        let previous_safeuser = env::var_os("SAFEUSER");
        unsafe { env::set_var("SAFEUSER", "tester"); }

        let request = CommitPushPrRequest {
            commit_message: Some("feat: add feature file".to_string()),
            pr_title: "Add feature file".to_string(),
            pr_body: "## Summary\n- add feature file".to_string(),
            branch_name_hint: "Add feature file".to_string(),
        };

        // when
        let report =
            handle_commit_push_pr_slash_command(&request, &repo).expect("commit-push-pr succeeds");
        let branch = run_command(&repo, "git", &["branch", "--show-current"]);
        let message = run_command(&repo, "git", &["log", "-1", "--pretty=%B"]);
        let gh_invocations = fs::read_to_string(&gh_log).expect("gh log should exist");

        // then
        assert!(report.contains("Result           created"));
        assert!(report.contains("URL              https://example.com/pr/123"));
        assert_eq!(branch.trim(), "tester/add-feature-file");
        assert_eq!(message.trim(), "feat: add feature file");
        assert!(gh_invocations.contains("pr create"));
        assert!(gh_invocations.contains("--base main"));

        if let Some(path) = previous_path {
            unsafe { env::set_var("PATH", path); }
        } else {
            unsafe { env::remove_var("PATH"); }
        }
        if let Some(safeuser) = previous_safeuser {
            unsafe { env::set_var("SAFEUSER", safeuser); }
        } else {
            unsafe { env::remove_var("SAFEUSER"); }
        }

        let _ = fs::remove_dir_all(repo);
        let _ = fs::remove_dir_all(remote);
        let _ = fs::remove_dir_all(fake_bin);
    }

    // ── Phase 0 v2.2.6 tests ─────────────────────────────────────────────────

    /// Exhaustiveness: every SlashCommand variant must have a matching spec entry.
    ///
    /// This test intentionally uses an exhaustive match so that the compiler
    /// forces a spec update whenever a new variant is added.  If you add a
    /// variant, add the corresponding spec and extend the set below.
    #[test]
    fn every_slash_command_variant_has_a_spec() {
        use super::{slash_command_specs, SlashCommand};

        // Walk every variant and collect the primary name.
        // New variants MUST be added here — no `_` arm.
        // Returns "" for Unknown (no spec by design).
        fn variant_name(cmd: &SlashCommand) -> &'static str {
            match cmd {
                SlashCommand::Help { .. } => "help",
                SlashCommand::Status => "status",
                SlashCommand::Compact => "compact",
                SlashCommand::Branch { .. } => "branch",
                SlashCommand::Bughunter { .. } => "bughunter",
                SlashCommand::Worktree { .. } => "worktree",
                SlashCommand::Commit => "commit",
                SlashCommand::CommitPushPr { .. } => "commit-push-pr",
                SlashCommand::Pr { .. } => "pr",
                SlashCommand::Issue { .. } => "issue",
                SlashCommand::Ultraplan { .. } => "ultraplan",
                SlashCommand::Teleport { .. } => "teleport",
                SlashCommand::DebugToolCall => "debug-tool-call",
                SlashCommand::Model { .. } => "model",
                SlashCommand::Permissions { .. } => "permissions",
                SlashCommand::Clear { .. } => "clear",
                SlashCommand::Cost => "cost",
                SlashCommand::Resume { .. } => "resume",
                SlashCommand::Config { .. } => "config",
                SlashCommand::Memory { .. } => "memory",
                SlashCommand::Init => "init",
                SlashCommand::Diff => "diff",
                SlashCommand::Version => "version",
                SlashCommand::Export { .. } => "export",
                SlashCommand::Session { .. } => "session",
                SlashCommand::Plugins { .. } => "plugin",
                SlashCommand::Agents { .. } => "agents",
                SlashCommand::Skills { .. } => "skills",
                SlashCommand::Qmd { .. } => "qmd",
                SlashCommand::Undo => "undo",
                SlashCommand::History { .. } => "history",
                SlashCommand::Context { .. } => "context",
                SlashCommand::Pin { .. } => "pin",
                SlashCommand::Unpin { .. } => "unpin",
                SlashCommand::Chat => "chat",
                SlashCommand::Vim => "vim",
                SlashCommand::Web { .. } => "web",
                SlashCommand::Doctor { .. } => "doctor",
                SlashCommand::Tokens => "tokens",
                SlashCommand::Provider { .. } => "provider",
                SlashCommand::Login { .. } => "login",
                SlashCommand::Search { .. } => "search",
                SlashCommand::Failover { .. } => "failover",
                SlashCommand::GenerateImage { .. } => "generate-image",
                SlashCommand::HistoryArchive { .. } => "history-archive",
                SlashCommand::Configure { .. } => "configure",
                SlashCommand::Theme { .. } => "theme",
                SlashCommand::SemanticSearch { .. } => "semantic-search",
                SlashCommand::Docker { .. } => "docker",
                SlashCommand::Test { .. } => "test",
                SlashCommand::Git { .. } => "git",
                SlashCommand::Refactor { .. } => "refactor",
                SlashCommand::Screenshot => "screenshot",
                SlashCommand::Db { .. } => "db",
                SlashCommand::Security { .. } => "security",
                SlashCommand::Api { .. } => "api",
                SlashCommand::Docs { .. } => "docs",
                SlashCommand::Scaffold { .. } => "scaffold",
                SlashCommand::Perf { .. } => "perf",
                SlashCommand::Debug { .. } => "debug",
                SlashCommand::Voice { .. } => "voice",
                SlashCommand::Collab { .. } => "collab",
                SlashCommand::Changelog => "changelog",
                SlashCommand::Env { .. } => "env",
                SlashCommand::Hub { .. } => "hub",
                SlashCommand::Language { .. } => "language",
                SlashCommand::Lsp { .. } => "lsp",
                SlashCommand::Notebook { .. } => "notebook",
                SlashCommand::K8s { .. } => "k8s",
                SlashCommand::Iac { .. } => "iac",
                SlashCommand::Pipeline { .. } => "pipeline",
                SlashCommand::Review { .. } => "review",
                SlashCommand::Deps { .. } => "deps",
                SlashCommand::Mono { .. } => "mono",
                SlashCommand::Browser { .. } => "browser",
                SlashCommand::Notify { .. } => "notify",
                SlashCommand::Vault { .. } => "vault",
                SlashCommand::Migrate { .. } => "migrate",
                SlashCommand::Regex { .. } => "regex",
                SlashCommand::Ssh { .. } => "ssh",
                SlashCommand::Logs { .. } => "logs",
                SlashCommand::Markdown { .. } => "markdown",
                SlashCommand::Snippets { .. } => "snippets",
                SlashCommand::Finetune { .. } => "finetune",
                SlashCommand::Webhook { .. } => "webhook",
                SlashCommand::PluginSdk { .. } => "plugin-sdk",
                SlashCommand::Sleep => "sleep",
                SlashCommand::Think => "think",
                SlashCommand::Fast => "fast",
                SlashCommand::ReviewPr { .. } => "review-pr",
                SlashCommand::RemoteControl { .. } => "remote-control",
                SlashCommand::Loop { .. } => "loop",
                SlashCommand::Focus => "focus",
                SlashCommand::Mcp { .. } => "mcp",
                SlashCommand::Productivity => "productivity",
                SlashCommand::Knowledge { .. } => "knowledge",
                SlashCommand::Daily { .. } => "daily",
                // Ghost commands promoted in v2.2.6:
                SlashCommand::Tab { .. } => "tab",
                SlashCommand::Fork => "fork",
                SlashCommand::Share { .. } => "share",
                SlashCommand::Audit => "audit",
                // Respawn mechanism (v2.2.6 Phase 5):
                SlashCommand::Restart { .. } => "restart",
                // Agent composition (v2.2.7):
                SlashCommand::Agent { .. } => "agent",
                // Output style axis (v2.2.8):
                SlashCommand::OutputStyle { .. } => "output-style",
                // Effort/reasoning slider (v2.2.11):
                SlashCommand::Effort { .. } => "effort",
                // Skill dispatch (v2.2.7):
                SlashCommand::Skill { .. } => "skill",
                // Goal tracking (v2.2.11):
                SlashCommand::Goal { .. } => "goal",
                SlashCommand::FileCache { .. } => "file-cache",
                SlashCommand::CmdCache { .. } => "cmd-cache",
                // Named profiles (v2.2.11 W4):
                SlashCommand::Profile { .. } => "profile",
                SlashCommand::Unknown(_) => "", // unknown has no spec by design
            }
        }

        // Build a representative instance of each variant.
        // variant_name returns "" for Unknown — we skip those.
        let variants: Vec<SlashCommand> = vec![
            SlashCommand::Help { command: None },
            SlashCommand::Status,
            SlashCommand::Compact,
            SlashCommand::Branch { action: None, target: None },
            SlashCommand::Bughunter { scope: None },
            SlashCommand::Worktree { action: None, path: None, branch: None },
            SlashCommand::Commit,
            SlashCommand::CommitPushPr { context: None },
            SlashCommand::Pr { context: None },
            SlashCommand::Issue { context: None },
            SlashCommand::Ultraplan { task: None },
            SlashCommand::Teleport { target: None },
            SlashCommand::DebugToolCall,
            SlashCommand::Model { model: None },
            SlashCommand::Permissions { mode: None },
            SlashCommand::Clear { confirm: false, all_tabs: false },
            SlashCommand::Cost,
            SlashCommand::Resume { session_path: None },
            SlashCommand::Config { section: None },
            SlashCommand::Memory { action: None },
            SlashCommand::Init,
            SlashCommand::Diff,
            SlashCommand::Version,
            SlashCommand::Export { format: None, path: None },
            SlashCommand::Session { action: None, target: None },
            SlashCommand::Plugins { action: None, target: None },
            SlashCommand::Agents { args: None },
            SlashCommand::Skills { args: None },
            SlashCommand::Qmd { query: None },
            SlashCommand::Undo,
            SlashCommand::History { show_all: false },
            SlashCommand::Context { path: None },
            SlashCommand::Pin { path: None },
            SlashCommand::Unpin { path: String::new() },
            SlashCommand::Chat,
            SlashCommand::Vim,
            SlashCommand::Web { query: String::new() },
            SlashCommand::Doctor { mode: None },
            SlashCommand::Tokens,
            SlashCommand::Provider { action: None },
            SlashCommand::Login { provider: None },
            SlashCommand::Search { args: None },
            SlashCommand::Failover { action: None },
            SlashCommand::GenerateImage { prompt: String::new(), wp_post_id: None },
            SlashCommand::HistoryArchive { action: None },
            SlashCommand::Configure { args: None },
            SlashCommand::Theme { action: None },
            SlashCommand::SemanticSearch { args: None },
            SlashCommand::Docker { action: None },
            SlashCommand::Test { action: None },
            SlashCommand::Git { action: None },
            SlashCommand::Refactor { action: None },
            SlashCommand::Screenshot,
            SlashCommand::Db { action: None },
            SlashCommand::Security { action: None },
            SlashCommand::Api { action: None },
            SlashCommand::Docs { action: None },
            SlashCommand::Scaffold { action: None },
            SlashCommand::Perf { action: None },
            SlashCommand::Debug { action: None },
            SlashCommand::Voice { action: None },
            SlashCommand::Collab { action: None },
            SlashCommand::Changelog,
            SlashCommand::Env { action: None },
            SlashCommand::Hub { action: None },
            SlashCommand::Language { lang: None },
            SlashCommand::Lsp { action: None },
            SlashCommand::Notebook { action: None },
            SlashCommand::K8s { action: None },
            SlashCommand::Iac { action: None },
            SlashCommand::Pipeline { action: None },
            SlashCommand::Review { action: None },
            SlashCommand::Deps { action: None },
            SlashCommand::Mono { action: None },
            SlashCommand::Browser { action: None },
            SlashCommand::Notify { action: None },
            SlashCommand::Vault { action: None },
            SlashCommand::Migrate { action: None },
            SlashCommand::Regex { action: None },
            SlashCommand::Logs { action: None },
            SlashCommand::Markdown { action: None },
            SlashCommand::Snippets { action: None },
            SlashCommand::Finetune { action: None },
            SlashCommand::Webhook { action: None },
            SlashCommand::Ssh { args: None },
            SlashCommand::PluginSdk { action: None },
            SlashCommand::Sleep,
            SlashCommand::Think,
            SlashCommand::Fast,
            SlashCommand::ReviewPr { number: None },
            SlashCommand::RemoteControl { action: None },
            SlashCommand::Loop { prompt: None },
            SlashCommand::Focus,
            SlashCommand::Mcp { action: None },
            SlashCommand::Productivity,
            SlashCommand::Knowledge { action: None },
            SlashCommand::Daily { date: None },
            SlashCommand::Tab { action: None },
            SlashCommand::Fork,
            SlashCommand::Share { action: None },
            SlashCommand::Audit,
            SlashCommand::Restart { soft: false },
            SlashCommand::Agent { subcommand: AgentSubcommand::Traits },
            SlashCommand::OutputStyle { style: None },
            SlashCommand::Effort { level: None },
            SlashCommand::Skill { subcommand: SkillSubcommand::List },
            SlashCommand::Goal { action: None },
            // Named profiles (v2.2.11 W4):
            SlashCommand::FileCache { action: None },
            SlashCommand::CmdCache { action: None },
            SlashCommand::Profile { action: None },
        ];

        let specs = slash_command_specs();
        let spec_names: std::collections::HashSet<&str> =
            specs.iter().map(|s| s.name).collect();

        for variant in &variants {
            let name = variant_name(variant);
            if name.is_empty() {
                continue; // Unknown variant — no spec by design
            }
            assert!(
                spec_names.contains(name),
                "SlashCommand variant has no matching spec: '{name}'"
            );
        }
    }

    /// Verify suggest_completions returns root commands when input is empty.
    #[test]
    fn completion_root_returns_all_commands_for_empty_input() {
        use super::{suggest_completions, NoopCompletionContext};
        let ctx = NoopCompletionContext;
        let completions = suggest_completions("/", &ctx);
        assert!(completions.len() >= 90, "expected at least 90 root completions, got {}", completions.len());
        assert!(completions.iter().any(|c| c.text == "/vault"));
        assert!(completions.iter().any(|c| c.text == "/help"));
    }

    /// Verify prefix filtering at root level.
    #[test]
    fn completion_root_filters_by_prefix() {
        use super::{suggest_completions, NoopCompletionContext};
        let ctx = NoopCompletionContext;
        let completions = suggest_completions("/va", &ctx);
        assert!(completions.iter().all(|c| c.text.starts_with("/va")));
        assert!(completions.iter().any(|c| c.text == "/vault"));
    }

    /// Verify /vault <space> returns the vault subcommands.
    #[test]
    fn completion_vault_subcommands() {
        use super::{suggest_completions, NoopCompletionContext};
        let ctx = NoopCompletionContext;
        let completions = suggest_completions("/vault ", &ctx);
        let names: Vec<&str> = completions.iter().map(|c| c.text.as_str()).collect();
        assert!(names.contains(&"store"), "expected 'store' in {:?}", names);
        assert!(names.contains(&"unlock"));
        assert!(names.contains(&"list"));
        assert!(names.contains(&"get"));
    }

    /// Verify /vault store <space> returns the credential types.
    #[test]
    fn completion_vault_store_returns_credential_types() {
        use super::{suggest_completions, NoopCompletionContext};
        let ctx = NoopCompletionContext;
        let completions = suggest_completions("/vault store ", &ctx);
        let names: Vec<&str> = completions.iter().map(|c| c.text.as_str()).collect();
        assert!(names.contains(&"api_key"), "expected 'api_key' in {:?}", names);
        assert!(names.contains(&"ssh_key"));
        assert!(names.contains(&"totp"));
        assert_eq!(names.len(), 21, "expected all 21 credential types");
    }

    /// Verify /mcp <space> returns its subcommands.
    #[test]
    fn completion_mcp_subcommands() {
        use super::{suggest_completions, NoopCompletionContext};
        let ctx = NoopCompletionContext;
        let completions = suggest_completions("/mcp ", &ctx);
        let names: Vec<&str> = completions.iter().map(|c| c.text.as_str()).collect();
        assert!(names.contains(&"list"), "expected 'list' in {:?}", names);
        assert!(names.contains(&"status"));
        assert!(names.contains(&"tools"));
    }

    /// Verify /theme set <space> returns dynamic themes (empty with noop ctx).
    #[test]
    fn completion_theme_set_dynamic() {
        use super::{suggest_completions, NoopCompletionContext};
        let ctx = NoopCompletionContext;
        let completions = suggest_completions("/theme set ", &ctx);
        // With NoopCompletionContext, InstalledThemes returns []
        assert!(completions.is_empty(), "expected empty with noop ctx, got {:?}", completions);
    }

    /// Verify /theme set returns dynamic themes with the static default context.
    #[test]
    fn completion_theme_set_static_default_context() {
        use super::{suggest_completions, StaticDefaultCompletionContext};
        let ctx = StaticDefaultCompletionContext;
        let completions = suggest_completions("/theme set ", &ctx);
        let names: Vec<&str> = completions.iter().map(|c| c.text.as_str()).collect();
        assert!(names.contains(&"dark"), "expected 'dark' in {:?}", names);
        assert!(names.contains(&"light"));
    }

    /// Verify every DynamicEnumSource resolves without panic using NoopCompletionContext.
    #[test]
    fn noop_completion_context_resolves_all_sources() {
        use super::{DynamicEnumSource, NoopCompletionContext, CompletionContext};
        let ctx = NoopCompletionContext;
        let sources = [
            DynamicEnumSource::VaultCredentialTypes,
            DynamicEnumSource::InstalledPlugins,
            DynamicEnumSource::InstalledThemes,
            DynamicEnumSource::InstalledAgents,
            DynamicEnumSource::InstalledSkills,
            DynamicEnumSource::McpServers,
            DynamicEnumSource::Sessions,
            DynamicEnumSource::Models,
            DynamicEnumSource::Providers,
            DynamicEnumSource::Languages,
        ];
        for source in sources {
            let result = ctx.resolve(source);
            assert!(result.is_empty(), "noop ctx should return empty for {source:?}");
        }
    }

    /// Verify every DynamicEnumSource resolves with the static default context.
    #[test]
    fn static_default_context_resolves_all_sources() {
        use super::{DynamicEnumSource, StaticDefaultCompletionContext, CompletionContext};
        let ctx = StaticDefaultCompletionContext;
        let sources = [
            DynamicEnumSource::VaultCredentialTypes,
            DynamicEnumSource::InstalledThemes,
            DynamicEnumSource::Models,
            DynamicEnumSource::Providers,
            DynamicEnumSource::Languages,
        ];
        for source in sources {
            let result = ctx.resolve(source);
            assert!(!result.is_empty(), "static ctx should return values for {source:?}");
        }
    }

    /// Verify ghost commands parse correctly.
    #[test]
    fn ghost_commands_parse_correctly() {
        assert_eq!(
            SlashCommand::parse("/tab new"),
            Some(SlashCommand::Tab { action: Some("new".to_string()) })
        );
        assert_eq!(SlashCommand::parse("/fork"), Some(SlashCommand::Fork));
        assert_eq!(
            SlashCommand::parse("/share stop"),
            Some(SlashCommand::Share { action: Some("stop".to_string()) })
        );
        assert_eq!(SlashCommand::parse("/audit"), Some(SlashCommand::Audit));
    }

    /// Verify completion of ghost commands.
    #[test]
    fn completion_ghost_commands_have_subcommands() {
        use super::{suggest_completions, NoopCompletionContext};
        let ctx = NoopCompletionContext;
        let tab_completions = suggest_completions("/tab ", &ctx);
        let names: Vec<&str> = tab_completions.iter().map(|c| c.text.as_str()).collect();
        assert!(names.contains(&"new"), "expected 'new' in tab completions: {names:?}");
        assert!(names.contains(&"list"));
        assert!(names.contains(&"close"));

        let share_completions = suggest_completions("/share ", &ctx);
        let share_names: Vec<&str> = share_completions.iter().map(|c| c.text.as_str()).collect();
        assert!(share_names.contains(&"stop"), "expected 'stop' in share completions: {share_names:?}");
    }

    // ── /skill command parser tests ───────────────────────────────────────────

    #[test]
    fn skill_suggest_with_quoted_prompt_parses() {
        let cmd = SlashCommand::parse(r#"/skill suggest "audit my code""#);
        assert!(
            matches!(
                cmd,
                Some(SlashCommand::Skill {
                    subcommand: SkillSubcommand::Suggest { prompt: Some(ref p) }
                }) if p == "audit my code"
            ),
            "expected Skill::Suggest with prompt, got: {cmd:?}"
        );
    }

    #[test]
    fn skill_suggest_bare_parses_with_no_prompt() {
        let cmd = SlashCommand::parse("/skill suggest");
        assert!(
            matches!(
                cmd,
                Some(SlashCommand::Skill {
                    subcommand: SkillSubcommand::Suggest { prompt: None }
                })
            ),
            "expected Skill::Suggest with no prompt, got: {cmd:?}"
        );
    }

    #[test]
    fn skill_bare_parses_as_suggest_with_no_prompt() {
        // `/skill` with no subcommand defaults to suggest with no prompt.
        let cmd = SlashCommand::parse("/skill");
        assert!(
            matches!(
                cmd,
                Some(SlashCommand::Skill {
                    subcommand: SkillSubcommand::Suggest { prompt: None }
                })
            ),
            "expected Skill::Suggest (no prompt) for bare /skill, got: {cmd:?}"
        );
    }

    #[test]
    fn skill_load_parses_with_name() {
        let cmd = SlashCommand::parse("/skill load security-audit");
        assert!(
            matches!(
                cmd,
                Some(SlashCommand::Skill {
                    subcommand: SkillSubcommand::Load { ref name }
                }) if name == "security-audit"
            ),
            "expected Skill::Load {{ name: security-audit }}, got: {cmd:?}"
        );
    }

    #[test]
    fn skill_list_parses() {
        let cmd = SlashCommand::parse("/skill list");
        assert!(
            matches!(cmd, Some(SlashCommand::Skill { subcommand: SkillSubcommand::List })),
            "expected Skill::List, got: {cmd:?}"
        );
    }

    // ── format_suggestions tests ──────────────────────────────────────────────

    #[test]
    fn format_suggestions_with_matches_produces_grouped_listing() {
        use super::skill_triggers::{match_triggers, TriggerMatch};
        let matches = vec![
            TriggerMatch {
                skill_name: "security-audit".to_string(),
                matched_trigger: "audit".to_string(),
            },
            TriggerMatch {
                skill_name: "code-review".to_string(),
                matched_trigger: "review".to_string(),
            },
        ];
        let output = super::format_suggestions(&matches, "audit auth.rs");
        assert!(output.contains("security-audit"), "missing security-audit in: {output}");
        assert!(output.contains("/skill load security-audit"), "missing /skill load hint in: {output}");
        assert!(output.contains("audit"), "missing matched trigger in: {output}");
    }

    #[test]
    fn format_suggestions_empty_returns_no_matches_message() {
        let output = super::format_suggestions(&[], "unrelated prompt");
        assert!(output.contains("No skill suggestions"), "expected 'No skill suggestions' in: {output}");
    }

    // ── format_suggestions_hint tests ────────────────────────────────────────

    #[test]
    fn format_suggestions_hint_fires_when_matches_exist() {
        use super::skill_triggers::TriggerMatch;
        let matches = vec![TriggerMatch {
            skill_name: "security-audit".to_string(),
            matched_trigger: "audit".to_string(),
        }];
        let hint = super::format_suggestions_hint(&matches);
        assert!(hint.is_some(), "expected Some hint");
        let hint = hint.unwrap();
        assert!(hint.contains("security-audit"), "missing skill name in: {hint}");
        assert!(hint.contains("/skill load"), "missing /skill load in: {hint}");
    }

    #[test]
    fn format_suggestions_hint_is_silent_when_no_matches() {
        let hint = super::format_suggestions_hint(&[]);
        assert!(hint.is_none(), "expected None when no matches");
    }

    #[test]
    fn skill_load_unknown_name_message() {
        // Ensure format_suggestions still works without a real skill root
        // (no disk I/O needed for this unit test path).
        let output = super::format_suggestions(&[], "some prompt");
        assert!(!output.is_empty());
    }

    // T5-Ssh-A: parser variants for the embedded SSH client slash command.
    #[test]
    fn ssh_bare_command_parses_with_no_args() {
        assert_eq!(
            SlashCommand::parse("/ssh"),
            Some(SlashCommand::Ssh { args: None }),
        );
    }

    #[test]
    fn ssh_with_alias_parses_alias_into_args() {
        assert_eq!(
            SlashCommand::parse("/ssh guard"),
            Some(SlashCommand::Ssh {
                args: Some("guard".to_string()),
            }),
        );
    }

    #[test]
    fn ssh_save_subcommand_parses_into_args() {
        assert_eq!(
            SlashCommand::parse("/ssh save myalias"),
            Some(SlashCommand::Ssh {
                args: Some("save myalias".to_string()),
            }),
        );
    }

    #[test]
    fn ssh_handles_extra_whitespace() {
        assert_eq!(
            SlashCommand::parse("/ssh   guard"),
            Some(SlashCommand::Ssh {
                args: Some("guard".to_string()),
            }),
        );
    }
}
