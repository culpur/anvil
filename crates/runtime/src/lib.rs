mod bash;
mod bootstrap;
pub mod ssh;
pub mod auto_promote;
pub mod effort;
pub mod session_ctx;
pub mod requirements;
pub mod goals;
pub mod hub;
pub mod otel;
pub mod share;
pub mod theme;
pub mod cron;
pub mod daily;
pub mod history;
pub mod memory;
pub mod nominations;
pub mod private_memory;
pub mod qmd;
pub mod routines;
pub mod scroll_speed;
pub mod agent_ctx;
pub mod tool_pattern;
pub mod search;
pub mod task;
pub mod team;
pub mod audit;
pub mod egress;
pub mod vault;
pub mod vault_session;
pub mod file_cache;
pub mod command_cache;
mod compact;
mod config;
mod content_filter;
mod conversation;
mod file_ops;
mod hooks;
mod json;
mod keybindings;
mod mcp;
mod mcp_client;
mod mcp_stdio;
mod oauth;
mod permission_memory;
mod permissions;
mod prompt;
pub mod relay;
mod remote;
pub mod sandbox;
mod session;
mod usage;

pub use lsp::{
    FileDiagnostics, LspContextEnrichment, LspError, LspManager, LspServerConfig,
    SymbolLocation, WorkspaceDiagnostics,
};
pub use bash::{execute_bash, BashCommandInput, BashCommandOutput};
pub use bootstrap::{BootstrapPhase, BootstrapPlan};
pub use compact::{
    compact_session, estimate_session_tokens, format_compact_summary,
    get_compact_continuation_message, should_compact, CompactionConfig, CompactionResult,
};
pub use config::{
    default_config_home, ConfigEntry, ConfigError, ConfigLoader, ConfigSource, LspConfig,
    LspServerEntry, McpManagedProxyServerConfig, McpConfigCollection, McpOAuthConfig,
    McpRemoteServerConfig, McpSdkServerConfig, McpServerConfig, McpStdioServerConfig,
    McpTransport, McpWebSocketServerConfig, OAuthConfig, OtelConfig,
    BuiltInStyle, CustomStyle, OutputStyle, OutputStyleRegistry, default_output_styles_dir, output_style_from_str_builtin_only,
    PolicyCheckError, ResolvedPermissionMode, RuntimeConfig, RuntimeFeatureConfig,
    RuntimeHookConfig, RuntimePluginConfig, ScopedMcpServerConfig, WorktreeConfig,
    ANVIL_SETTINGS_SCHEMA_NAME,
};
pub use effort::{resolve_effort, resolve_effort_from_env, EffortLevel};
pub use config::schema::{emit_schema as emit_config_schema, write_schema_to as write_config_schema_to};
pub use conversation::{
    ApiClient, ApiRequest, AssistantEvent, ConversationRuntime, RuntimeError, StaticToolExecutor,
    ToolError, ToolExecutor, TurnSummary,
};
pub use file_ops::{
    active_sandbox_mode, edit_file, glob_search, grep_search, read_file, set_active_sandbox_mode,
    write_file, EditFileOutput, GlobSearchOutput, GrepSearchInput, GrepSearchOutput,
    ReadFileOutput, SandboxMode, StructuredPatchHunk, TextFilePayload, WriteFileOutput,
};
pub use hooks::{
    CwdChangedPayload, FileChangeAction, FileChangedPayload, HookEvent, HookPermissionDecision,
    HookRunResult, HookRunner, McpHookInvocationResult, McpHookInvoker, NotificationKind,
    NotificationPayload, PermissionDeniedPayload, PermissionDeniedSource,
    PermissionRequestHookResult, PermissionRequestPayload, PostToolBatchPayload, RuntimeHookSpec,
};
pub use mcp::{
    mcp_server_signature, mcp_tool_name, mcp_tool_prefix, normalize_name_for_mcp,
    scoped_mcp_config_hash, unwrap_ccr_proxy_url,
};
pub use mcp_client::{
    McpManagedProxyTransport, McpClientAuth, McpClientBootstrap, McpClientTransport,
    McpRemoteTransport, McpSdkTransport, McpStdioTransport,
};
pub use mcp_stdio::{
    spawn_mcp_stdio_process, JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse,
    ManagedMcpTool, McpInitializeClientInfo, McpInitializeParams, McpInitializeResult,
    McpInitializeServerInfo, McpListResourcesParams, McpListResourcesResult, McpListToolsParams,
    McpListToolsResult, McpReadResourceParams, McpReadResourceResult, McpResource,
    McpResourceContents, McpServerManager, McpServerManagerError, McpStdioProcess, McpTool,
    McpToolCallContent, McpToolCallParams, McpToolCallResult, UnsupportedMcpServer,
};
pub use oauth::{
    clear_oauth_credentials, code_challenge_s256, credentials_path, generate_pkce_pair,
    generate_state, load_oauth_credentials, loopback_redirect_uri, parse_oauth_callback_query,
    parse_oauth_callback_request_target, parse_pasted_oauth_code, save_oauth_credentials,
    OAuthAuthorizationRequest, OAuthCallbackParams, OAuthRefreshRequest,
    OAuthTokenExchangeRequest, OAuthTokenSet, PkceChallengeMethod, PkceCodePair,
};
pub use permissions::{
    BlockAction, PermissionMode, PermissionOutcome, PermissionPolicy, PermissionPromptDecision,
    PermissionPrompter, PermissionRequest, ReviewResult, ReviewerConfig, ReviewerMode,
};
pub use permissions::reviewer::{Recommendation, Reviewer};
pub use permission_memory::{PermissionMemory, PermissionMemoryEntry, PermissionScope};
pub use keybindings::KeybindingsConfig;
pub use content_filter::{ContentFilter, ContentFilterConfig, FilterResult, FilterSeverity};
pub use prompt::{
    load_system_prompt, load_system_prompt_with_identity, prepend_bullets, ANVIL_VERSION, ContextFile, ProjectContext, PromptBuildError,
    SystemPromptBuilder, FRONTIER_MODEL_NAME, SYSTEM_PROMPT_DYNAMIC_BOUNDARY,
};
pub use remote::{
    inherited_upstream_proxy_env, no_proxy_list, read_token, upstream_proxy_ws_url,
    RemoteSessionContext, UpstreamProxyBootstrap, UpstreamProxyState, DEFAULT_REMOTE_BASE_URL,
    DEFAULT_SESSION_TOKEN_PATH, DEFAULT_SYSTEM_CA_BUNDLE, NO_PROXY_HOSTS, UPSTREAM_PROXY_ENV_KEYS,
};
pub use session::{ContentBlock, ConversationMessage, MessageRole, Session, SessionError};
pub use cron::{next_run_time, CronDaemon, CronEntry, CronManager};
pub use daily::{
    extract_tasks, today_date, DailySummary, DailyStore, SessionSummary,
};
pub use task::{CompletedTaskInfo, TaskManager, TaskStatus};
pub use team::{
    DelegationRecord, MemberStatus, TaskSnapshot, TeamManager, TeamMember, TeamStatus,
};
pub use memory::{memory_dir_for_project, project_path_hash, MemoryFile, MemoryManager, MemoryType};
pub use scroll_speed::{get_scroll_speed, reset_scroll_speed, set_scroll_speed};
pub use auto_promote::{
    install_default as install_auto_promote_default, install_global as install_auto_promote,
    is_installed as auto_promote_is_installed, observe as auto_promote_observe,
    stats as auto_promote_stats, AccessKind, AutoPromoteError, AutoPromoter, AutoPromoterStats,
    ObservedAccess,
};
pub use goals::{
    build_active_goal_prompt_fragment, format_goal_list, format_goal_show,
    Goal, GoalError, GoalManager, GoalStatus,
    GOAL_DESCRIPTION_MAX, GOAL_LIST_DESCRIPTION_TRUNCATE, GOAL_STATUS_LINE_TRUNCATE,
};
pub use private_memory::{private_memory_project_hash, PrivateProjectMemory};
pub use history::{ArchiveEntry, HistoryArchiver, MIN_ARCHIVE_MESSAGES};
pub use qmd::{render_history_context, render_qmd_context, QmdClient, QmdResult, QmdStatus};
pub use search::{
    format_provider_list, ProviderConfig as SearchProviderConfig, SearchConfig, SearchEngine,
    SearchProvider, SearchResult,
};
pub use usage::{
    format_usd, pricing_for_model, ModelPricing, TokenUsage, UsageCostEstimate, UsageTracker,
};
pub use hub::{
    format_package_detail, format_package_list, BlockingHubClient, HubClient, HubError,
    HubPackage,
};
pub use share::{
    scrub_secrets, ActiveShare, BlockingShareClient, ShareClient, ShareError, ShareMessage,
    ShareSnapshot,
};
pub use theme::{Rgb, Theme};
pub use vault::{Credential, CredentialType, TotpCode, TotpEntry, VaultError, VaultManager};
pub use vault_session::{
    init_session_vault, vault_is_initialized, vault_is_session_unlocked,
    vault_session_get, vault_session_upsert, with_session_vault, with_session_vault_mut,
};
pub use requirements::{
    check_plugin_install_policy, load_from_paths as load_requirements,
    validate as validate_requirements, PolicyViolation, RequirementsPolicy,
};
pub use file_cache::{
    build_known_files_block, forget_entry_best_effort, refresh_entry_best_effort, FileCacheEntry,
    FileCacheError, FileCacheManager, FileCacheStats, LARGE_FILE_THRESHOLD_BYTES, MAX_KEY_SYMBOLS,
    MAX_PROMPT_BYTES, MAX_PROMPT_ENTRIES, MAX_SUMMARY_LEN,
};
pub use command_cache::{
    is_cacheable as command_is_cacheable, default_ttl as command_cache_default_ttl,
    infer_touched_files as command_cache_infer_touched_files,
    CommandCacheEntry, CommandCacheError, CommandCacheManager, CommandCacheStats,
};

#[cfg(test)]
pub(crate) fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
