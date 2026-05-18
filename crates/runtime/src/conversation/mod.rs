pub mod permission_gate;
pub mod turn_executor;
pub mod usage_tracking;

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::compact::{
    compact_session, estimate_session_tokens, CompactionConfig, CompactionResult,
};
use crate::config::RuntimeFeatureConfig;
use crate::hooks::{
    CwdChangedPayload, HookRunResult, HookRunner, NotificationPayload,
};
use crate::permission_memory::PermissionMemory;
use crate::permissions::{PermissionPolicy, PermissionPrompter};
use crate::auto_mode::AutoModeConfig;
use crate::permissions::reviewer::Reviewer;
use crate::prompt_section::PromptSection;
use crate::session::{ContentBlock, ConversationMessage, Session};
use crate::usage::{TokenUsage, UsageTracker};

use turn_executor::run_turn_inner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRequest {
    /// Typed system-prompt sections (v2.2.14: replaced `Vec<String>`).
    ///
    /// API clients render this to the wire format with `.iter().map(|s|
    /// &s.body).cloned().collect::<Vec<_>>().join("\n\n")` (or equivalent).
    /// The runtime never inspects bodies — only kinds and labels.
    pub system_prompt: Vec<PromptSection>,
    pub messages: Vec<ConversationMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssistantEvent {
    TextDelta(String),
    ToolUse {
        id: String,
        name: String,
        input: String,
    },
    Usage(TokenUsage),
    MessageStop,
}

pub trait ApiClient {
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError>;

    /// Install a cooperative cancellation flag. Implementations that drive an
    /// SSE / chunked stream should poll the flag between frames and bail out
    /// with `RuntimeError::cancelled()` when it goes true. The default impl
    /// ignores the token, which keeps every existing test client compiling.
    fn set_cancel_token(&mut self, _token: Arc<AtomicBool>) {}
}

pub trait ToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    message: String,
}

impl ToolError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for ToolError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ToolError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeError {
    message: String,
    cancelled: bool,
    /// Task #564: provider returned a context-overflow error.  When set,
    /// the turn loop catches the error, runs reactive compaction, and
    /// retries the same turn (up to `reactive_max_retries`).  Carries
    /// the model-reported overflow in tokens; `0` when unknown.
    context_too_long_overflow: Option<u32>,
}

impl RuntimeError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cancelled: false,
            context_too_long_overflow: None,
        }
    }

    /// User-requested cancellation (Ctrl+C while a turn was streaming).
    #[must_use]
    pub fn cancelled() -> Self {
        Self {
            message: "turn cancelled".to_string(),
            cancelled: true,
            context_too_long_overflow: None,
        }
    }

    /// Task #564: provider reported a context-overflow error.  The
    /// caller (turn loop) catches this variant, runs reactive
    /// compaction, and retries the turn.  `overflow_tokens` is the
    /// model-reported overflow; pass `0` when unknown.
    #[must_use]
    pub fn context_too_long(overflow_tokens: u32) -> Self {
        Self {
            message: format!(
                "context too long: provider reported overflow of {overflow_tokens} tokens"
            ),
            cancelled: false,
            context_too_long_overflow: Some(overflow_tokens),
        }
    }

    /// True when the error came from a cooperative cancel rather than a real
    /// API/runtime failure. Callers use this to suppress the usual "Error:"
    /// surface and show a calmer "cancelled" indicator instead.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Task #564: returns `Some(overflow_tokens)` when the error came
    /// from a provider's context-overflow response.  Used by the turn
    /// loop to gate reactive compaction.
    #[must_use]
    pub const fn context_too_long_overflow(&self) -> Option<u32> {
        self.context_too_long_overflow
    }
}

impl Display for RuntimeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RuntimeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnSummary {
    pub assistant_messages: Vec<ConversationMessage>,
    pub tool_results: Vec<ConversationMessage>,
    pub iterations: usize,
    pub usage: TokenUsage,
}

pub struct ConversationRuntime<C, T> {
    session: Session,
    api_client: C,
    tool_executor: T,
    permission_policy: PermissionPolicy,
    system_prompt: Vec<PromptSection>,
    max_iterations: usize,
    usage_tracker: UsageTracker,
    hook_runner: HookRunner,
    /// W8 reviewer gate — compiled once from `ReviewerConfig`.
    reviewer: Reviewer,
    /// Auto-mode hard-deny list (CC-136-F2). Evaluated before hooks when
    /// the active permission mode is `WorkspaceWrite`.
    auto_mode: AutoModeConfig,
    /// L6 PermissionMemory store. Populated when
    /// `permissions.use_permission_memory` is true in settings.json AND a
    /// `project_dir` was passed to `new_with_features`. When `None`, the
    /// permission gate behaves exactly as it did before — every escalation
    /// reaches the prompter.
    permission_memory: Option<Arc<Mutex<PermissionMemory>>>,
    /// v2.2.14 TUI-1: shared cancel flag wired into the streaming loop.
    /// Cloned and installed on the `ApiClient` before every `stream()` call
    /// so the SSE loop can bail between frames. The turn loop also polls it
    /// between tool-use iterations. Flipped from outside (the TUI's Ctrl+C
    /// handler) through `cancel_handle`.
    cancel_token: Arc<AtomicBool>,
    /// Task #566: per-session Stop-hook block counter.  When a Stop hook
    /// returns `{"decision":"block"}` the runtime keeps the turn alive; the
    /// counter caps the number of consecutive blocks before the runtime
    /// emits a one-shot warning and lets the stop proceed.
    stop_hook_counter: crate::hooks::StopHookBlockCounter,
    /// Task #564: compaction configuration used by the proactive
    /// `compact()` helper AND by the turn loop's reactive
    /// context-overflow handler.  Initialised from
    /// `CompactionConfig::default()`; hosts can replace it via
    /// [`Self::set_compaction_config`].
    compaction_config: CompactionConfig,
}

impl<C, T> ConversationRuntime<C, T>
where
    C: ApiClient,
    T: ToolExecutor,
{
    #[must_use]
    pub fn new(
        session: Session,
        api_client: C,
        tool_executor: T,
        permission_policy: PermissionPolicy,
        system_prompt: Vec<PromptSection>,
    ) -> Self {
        Self::new_with_features(
            session,
            api_client,
            tool_executor,
            permission_policy,
            system_prompt,
            RuntimeFeatureConfig::default(),
        )
    }

    /// Construct a runtime with a typed feature configuration.
    ///
    /// L6 PermissionMemory is **not** wired here — without a project
    /// directory we can't compute the per-project storage path. Callers
    /// that want PermissionMemory (e.g. the CLI bootstrap) should use
    /// [`Self::new_with_features_and_project_dir`] instead and pass the
    /// project root.
    #[must_use]
    pub fn new_with_features(
        session: Session,
        api_client: C,
        tool_executor: T,
        permission_policy: PermissionPolicy,
        system_prompt: Vec<PromptSection>,
        feature_config: RuntimeFeatureConfig,
    ) -> Self {
        Self::new_with_features_and_project_dir(
            session,
            api_client,
            tool_executor,
            permission_policy,
            system_prompt,
            feature_config,
            None,
        )
    }

    /// Construct a runtime with a typed feature configuration and an
    /// optional project directory used to locate per-project state
    /// (currently L6 PermissionMemory).
    ///
    /// When `feature_config.permissions().use_permission_memory()` is true
    /// AND `project_dir` is `Some`, load `PermissionMemory` from disk and
    /// thread it into the permission gate. Otherwise leave it `None` —
    /// the gate falls back to the policy-only path.
    #[must_use]
    pub fn new_with_features_and_project_dir(
        session: Session,
        api_client: C,
        tool_executor: T,
        permission_policy: PermissionPolicy,
        system_prompt: Vec<PromptSection>,
        feature_config: RuntimeFeatureConfig,
        project_dir: Option<PathBuf>,
    ) -> Self {
        let usage_tracker = UsageTracker::from_session(&session);
        let reviewer = Reviewer::new(feature_config.reviewer());
        let auto_mode = feature_config.auto_mode().clone();
        let permission_memory = if feature_config.permissions().use_permission_memory() {
            project_dir.map(|dir| {
                let mem = PermissionMemory::load(&dir);
                Arc::new(Mutex::new(mem))
            })
        } else {
            None
        };
        Self {
            session,
            api_client,
            tool_executor,
            permission_policy,
            system_prompt,
            max_iterations: usize::MAX,
            usage_tracker,
            hook_runner: HookRunner::from_feature_config(&feature_config),
            reviewer,
            auto_mode,
            permission_memory,
            cancel_token: Arc::new(AtomicBool::new(false)),
            stop_hook_counter: crate::hooks::StopHookBlockCounter::default(),
            compaction_config: CompactionConfig::default(),
        }
    }

    /// Test/CLI helper: install an already-constructed PermissionMemory.
    /// Useful when the memory has been preloaded with synthetic grants or
    /// when the caller wants to share one store across multiple runtimes.
    pub fn set_permission_memory(&mut self, memory: Arc<Mutex<PermissionMemory>>) {
        self.permission_memory = Some(memory);
    }

    /// Inspector for tests / debugging.
    #[must_use]
    pub fn permission_memory(&self) -> Option<&Arc<Mutex<PermissionMemory>>> {
        self.permission_memory.as_ref()
    }

    /// Clone of the per-runtime cancel flag. External code (TUI Ctrl+C
    /// handler) flips this to `true`; the next inter-frame check inside the
    /// streaming loop short-circuits with `RuntimeError::cancelled()`.
    #[must_use]
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel_token)
    }

    /// Replace the cancel flag with an externally-owned one (typically the
    /// per-tab token held by the TUI). After this call, flipping either side
    /// of the shared `Arc` cancels the next inter-frame check.
    pub fn set_cancel_handle(&mut self, token: Arc<AtomicBool>) {
        self.cancel_token = token;
    }

    fn reset_cancel(&self) {
        self.cancel_token.store(false, Ordering::SeqCst);
    }

    #[must_use]
    pub const fn with_max_iterations(mut self, max_iterations: usize) -> Self {
        self.max_iterations = max_iterations;
        self
    }

    pub fn run_turn(
        &mut self,
        user_input: impl Into<String>,
        mut prompter: Option<&mut dyn PermissionPrompter>,
    ) -> Result<TurnSummary, RuntimeError> {
        self.session
            .messages
            .push(ConversationMessage::user_text(user_input.into()));
        self.run_turn_inner_dispatch(&mut prompter)
    }

    /// Run a model turn without prepending a new user message.  The caller is
    /// responsible for having already pushed the appropriate user-role content
    /// (e.g. via `inject_user_blocks`) before calling this.
    pub fn run_turn_preloaded(
        &mut self,
        mut prompter: Option<&mut dyn PermissionPrompter>,
    ) -> Result<TurnSummary, RuntimeError> {
        self.run_turn_inner_dispatch(&mut prompter)
    }

    fn run_turn_inner_dispatch(
        &mut self,
        prompter: &mut Option<&mut dyn PermissionPrompter>,
    ) -> Result<TurnSummary, RuntimeError> {
        self.reset_cancel();
        run_turn_inner(
            &mut self.session,
            &mut self.api_client,
            &mut self.tool_executor,
            &self.permission_policy,
            &self.system_prompt,
            self.max_iterations,
            &mut self.usage_tracker,
            &self.hook_runner,
            prompter,
            &self.reviewer,
            &self.auto_mode,
            self.permission_memory.as_ref(),
            &self.cancel_token,
            &mut self.stop_hook_counter,
            &self.compaction_config,
        )
    }

    /// Task #564: install a custom compaction config (proactive +
    /// reactive).  Hosts call this after construction when they need
    /// non-default settings (e.g. CLI bootstrap reading the user's
    /// `compaction.reactive_enabled` setting).
    pub fn set_compaction_config(&mut self, config: CompactionConfig) {
        self.compaction_config = config;
    }

    /// Task #557 (`/rewind`): replace the live session wholesale.  Used
    /// by the rewind handler when the user truncates the history; the
    /// next `run_turn` sees only the retained prefix.
    pub fn replace_session(&mut self, session: Session) {
        self.session = session;
    }

    #[must_use]
    pub const fn compaction_config(&self) -> &CompactionConfig {
        &self.compaction_config
    }

    /// Test/inspector access to the Stop-hook block counter (#566).
    #[must_use]
    pub fn stop_hook_counter(&self) -> &crate::hooks::StopHookBlockCounter {
        &self.stop_hook_counter
    }

    /// Test-only: install an externally-constructed HookRunner.  Used by
    /// the Stop-hook regression tests to inject `stop_extra` specs
    /// without round-tripping through settings.json.
    #[cfg(test)]
    pub fn set_hook_runner_for_testing(&mut self, runner: crate::hooks::HookRunner) {
        self.hook_runner = runner;
    }

    /// Test-only: lower the Stop-hook block cap so the force-stop path
    /// can be exercised in a small number of iterations.
    #[cfg(test)]
    pub fn set_stop_hook_cap_for_testing(&mut self, cap: u32) {
        self.stop_hook_counter = crate::hooks::StopHookBlockCounter::new(cap);
    }

    #[must_use]
    pub fn compact(&self, config: CompactionConfig) -> CompactionResult {
        compact_session(&self.session, config)
    }

    #[must_use]
    pub fn estimated_tokens(&self) -> usize {
        estimate_session_tokens(&self.session)
    }

    /// Per-tab autocompact threshold check (task #560).
    ///
    /// Returns `true` when this runtime's *own* `estimated_tokens()` is
    /// at or above `threshold_pct` of `context_max`. Reads no shared
    /// singleton — every tab calls this on its own `ConversationRuntime`
    /// so a tab at 90% triggers compaction without affecting a sibling
    /// tab at 30%. The threshold percentage and context window are
    /// passed in by the caller (the CLI reads them per-call from env +
    /// model metadata, also per-tab).
    #[must_use]
    pub fn should_auto_compact(&self, threshold_pct: usize, context_max: usize) -> bool {
        let threshold = context_max.saturating_mul(threshold_pct) / 100;
        self.estimated_tokens() >= threshold
    }

    /// Task #561: rebuild the in-session `HookRunner` from a freshly-loaded
    /// `RuntimeFeatureConfig` so that hook paths registered in the new
    /// project's `.anvil/settings.json` resolve against the new workspace
    /// root after `EnterWorktree` (or any other mid-session `cd`).
    ///
    /// The CLI invokes this after `EnterWorktree` /  `ExitWorktree` returns
    /// so that relative paths like `./hooks/preToolUse.sh` registered in
    /// the new workspace's settings.json fire correctly. Runtime-only
    /// `_extra` specs (e.g. McpTool entries injected by the host) are NOT
    /// preserved across the refresh — they live on the active runner
    /// because they were registered programmatically against a specific
    /// MCP invoker. If a session needs them across worktree boundaries,
    /// the host must re-register after refresh.
    pub fn refresh_hooks_from_feature_config(
        &mut self,
        feature_config: &RuntimeFeatureConfig,
    ) {
        let mcp_invoker = self.hook_runner.mcp_invoker_clone();
        let mut runner = HookRunner::from_feature_config(feature_config);
        if let Some(invoker) = mcp_invoker {
            runner = runner.with_mcp_invoker(invoker);
        }
        self.hook_runner = runner;
    }

    #[must_use]
    pub const fn usage(&self) -> &UsageTracker {
        &self.usage_tracker
    }

    #[must_use]
    pub const fn session(&self) -> &Session {
        &self.session
    }

    #[must_use]
    pub fn into_session(self) -> Session {
        self.session
    }

    /// Append a user-role text message directly to the session history without
    /// triggering a model turn.  This is used to inject out-of-band
    /// notifications (e.g. task completion events) so that the model sees them
    /// on its next turn.
    pub fn inject_user_message(&mut self, text: impl Into<String>) {
        self.session
            .messages
            .push(ConversationMessage::user_text(text.into()));
    }

    /// Append a user-role message with an arbitrary set of content blocks
    /// (e.g. text + image) without triggering a model turn.
    pub fn inject_user_blocks(&mut self, blocks: Vec<ContentBlock>) {
        self.session
            .messages
            .push(ConversationMessage::user_with_blocks(blocks));
    }

    /// T4-O: replace the cached system prompt wholesale. Used by the CLI
    /// hot-reload path when `ANVIL.md` or `MEMORY.md` changes on disk.
    pub fn replace_system_prompt(&mut self, prompt: Vec<PromptSection>) {
        self.system_prompt = prompt;
    }

    /// Read-only access to the live system-prompt vector.
    ///
    /// This is the source of truth for L1 working-memory introspection
    /// (`/memory show working`, `/memory why`, `/memory budget`). It is the
    /// exact sequence handed to the API client at turn-start by
    /// [`run_turn_inner`].
    #[must_use]
    pub fn system_prompt(&self) -> &[crate::PromptSection] {
        &self.system_prompt
    }

    /// Build a [`WorkingMemorySnapshot`] of the live L1 working memory.
    ///
    /// Phase 2 / Bucket 2 / L1 §4-5: the snapshot wraps the current
    /// `system_prompt` Vec so handlers can introspect what the model
    /// actually sees this turn — no static text, no guessing. The
    /// returned snapshot also serves the v2.2.14 daemon's resume path
    /// (it's the same struct).
    #[must_use]
    pub fn working_memory_snapshot(&self) -> crate::WorkingMemorySnapshot {
        crate::WorkingMemorySnapshot::new(self.system_prompt.clone())
    }

    // -----------------------------------------------------------------------
    // v2.2.11: hook dispatch helpers exposed to the TUI / CLI layer.
    // -----------------------------------------------------------------------

    /// Fire SessionStart hooks.  Returns any messages emitted by hooks.
    pub fn run_session_start_hooks(&self) -> Vec<String> {
        self.hook_runner.run_session_start().messages().to_vec()
    }

    /// Fire SessionEnd hooks.  Best-effort: errors degrade to warnings.
    pub fn run_session_end_hooks(&self) -> HookRunResult {
        self.hook_runner.run_session_end()
    }

    /// Fire CwdChanged hooks after the working directory changes.
    pub fn run_cwd_changed_hooks(&self, old_cwd: String, new_cwd: String) -> HookRunResult {
        self.hook_runner
            .run_cwd_changed(&CwdChangedPayload { old_cwd, new_cwd })
    }

    /// Fire Notification hooks when the TUI shows a notification to the user.
    pub fn run_notification_hooks(&self, payload: &NotificationPayload) -> HookRunResult {
        self.hook_runner.run_notification(payload)
    }
}

type ToolHandler = Box<dyn FnMut(&str) -> Result<String, ToolError>>;

#[derive(Default)]
pub struct StaticToolExecutor {
    handlers: BTreeMap<String, ToolHandler>,
}

impl StaticToolExecutor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn register(
        mut self,
        tool_name: impl Into<String>,
        handler: impl FnMut(&str) -> Result<String, ToolError> + 'static,
    ) -> Self {
        self.handlers.insert(tool_name.into(), Box::new(handler));
        self
    }
}

impl ToolExecutor for StaticToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        self.handlers
            .get_mut(tool_name)
            .ok_or_else(|| ToolError::new(format!("unknown tool: {tool_name}")))?(input)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ApiClient, ApiRequest, AssistantEvent, ConversationRuntime, RuntimeError,
        StaticToolExecutor, ToolExecutor,
    };
    use crate::compact::CompactionConfig;
    use crate::config::{RuntimeFeatureConfig, RuntimeHookConfig};
    use plugins::HookSpec;
    use crate::permissions::{
        PermissionMode, PermissionPolicy, PermissionPromptDecision, PermissionPrompter,
        PermissionRequest,
    };
    use crate::prompt::{ProjectContext, SystemPromptBuilder};
    use crate::prompt_section::{PromptSection, PromptSectionKind, PromptSectionsExt};
    use crate::session::{ContentBlock, ConversationMessage, MessageRole, Session};
    use crate::usage::TokenUsage;
    use std::path::PathBuf;

    struct ScriptedApiClient {
        call_count: usize,
    }

    impl ApiClient for ScriptedApiClient {
        fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
            self.call_count += 1;
            match self.call_count {
                1 => {
                    assert!(request
                        .messages
                        .iter()
                        .any(|message| message.role == MessageRole::User));
                    Ok(vec![
                        AssistantEvent::TextDelta("Let me calculate that.".to_string()),
                        AssistantEvent::ToolUse {
                            id: "tool-1".to_string(),
                            name: "add".to_string(),
                            input: "2,2".to_string(),
                        },
                        AssistantEvent::Usage(TokenUsage {
                            input_tokens: 20,
                            output_tokens: 6,
                            cache_creation_input_tokens: 1,
                            cache_read_input_tokens: 2,
                        }),
                        AssistantEvent::MessageStop,
                    ])
                }
                2 => {
                    let last_message = request
                        .messages
                        .last()
                        .expect("tool result should be present");
                    assert_eq!(last_message.role, MessageRole::Tool);
                    Ok(vec![
                        AssistantEvent::TextDelta("The answer is 4.".to_string()),
                        AssistantEvent::Usage(TokenUsage {
                            input_tokens: 24,
                            output_tokens: 4,
                            cache_creation_input_tokens: 1,
                            cache_read_input_tokens: 3,
                        }),
                        AssistantEvent::MessageStop,
                    ])
                }
                _ => Err(RuntimeError::new("unexpected extra API call")),
            }
        }
    }

    struct PromptAllowOnce;

    impl PermissionPrompter for PromptAllowOnce {
        fn decide(&mut self, request: &PermissionRequest) -> PermissionPromptDecision {
            assert_eq!(request.tool_name, "add");
            PermissionPromptDecision::Allow
        }
    }

    #[test]
    fn runs_user_to_tool_to_result_loop_end_to_end_and_tracks_usage() {
        let api_client = ScriptedApiClient { call_count: 0 };
        let tool_executor = StaticToolExecutor::new().register("add", |input| {
            let total = input
                .split(',')
                .map(|part| part.parse::<i32>().expect("input must be valid integer"))
                .sum::<i32>();
            Ok(total.to_string())
        });
        let permission_policy = PermissionPolicy::new(PermissionMode::WorkspaceWrite);
        let system_prompt = SystemPromptBuilder::new()
            .with_project_context(ProjectContext {
                cwd: PathBuf::from("/tmp/project"),
                current_date: "2026-03-31".to_string(),
                git_status: None,
                git_diff: None,
                instruction_files: Vec::new(),
            })
            .with_os("linux", "6.8")
            .build();
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            api_client,
            tool_executor,
            permission_policy,
            system_prompt,
        );

        let summary = runtime
            .run_turn("what is 2 + 2?", Some(&mut PromptAllowOnce))
            .expect("conversation loop should succeed");

        assert_eq!(summary.iterations, 2);
        assert_eq!(summary.assistant_messages.len(), 2);
        assert_eq!(summary.tool_results.len(), 1);
        assert_eq!(runtime.session().messages.len(), 4);
        assert_eq!(summary.usage.output_tokens, 10);
        assert!(matches!(
            runtime.session().messages[1].blocks[1],
            ContentBlock::ToolUse { .. }
        ));
        assert!(matches!(
            runtime.session().messages[2].blocks[0],
            ContentBlock::ToolResult {
                is_error: false,
                ..
            }
        ));
    }

    #[test]
    fn records_denied_tool_results_when_prompt_rejects() {
        struct RejectPrompter;
        impl PermissionPrompter for RejectPrompter {
            fn decide(&mut self, _request: &PermissionRequest) -> PermissionPromptDecision {
                PermissionPromptDecision::Deny {
                    reason: "not now".to_string(),
                }
            }
        }

        struct SingleCallApiClient;
        impl ApiClient for SingleCallApiClient {
            fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
                if request
                    .messages
                    .iter()
                    .any(|message| message.role == MessageRole::Tool)
                {
                    return Ok(vec![
                        AssistantEvent::TextDelta("I could not use the tool.".to_string()),
                        AssistantEvent::MessageStop,
                    ]);
                }
                Ok(vec![
                    AssistantEvent::ToolUse {
                        id: "tool-1".to_string(),
                        name: "blocked".to_string(),
                        input: "secret".to_string(),
                    },
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut runtime = ConversationRuntime::new(
            Session::new(),
            SingleCallApiClient,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::WorkspaceWrite),
            vec![PromptSection::new(PromptSectionKind::System, "system")],
        );

        let summary = runtime
            .run_turn("use the tool", Some(&mut RejectPrompter))
            .expect("conversation should continue after denied tool");

        assert_eq!(summary.tool_results.len(), 1);
        assert!(matches!(
            &summary.tool_results[0].blocks[0],
            ContentBlock::ToolResult { is_error: true, output, .. } if output == "not now"
        ));
    }

    #[test]
    fn denies_tool_use_when_pre_tool_hook_blocks() {
        struct SingleCallApiClient;
        impl ApiClient for SingleCallApiClient {
            fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
                if request
                    .messages
                    .iter()
                    .any(|message| message.role == MessageRole::Tool)
                {
                    return Ok(vec![
                        AssistantEvent::TextDelta("blocked".to_string()),
                        AssistantEvent::MessageStop,
                    ]);
                }
                Ok(vec![
                    AssistantEvent::ToolUse {
                        id: "tool-1".to_string(),
                        name: "blocked".to_string(),
                        input: r#"{"path":"secret.txt"}"#.to_string(),
                    },
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut runtime = ConversationRuntime::new_with_features(
            Session::new(),
            SingleCallApiClient,
            StaticToolExecutor::new().register("blocked", |_input| {
                panic!("tool should not execute when hook denies")
            }),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec![PromptSection::new(PromptSectionKind::System, "system")],
            RuntimeFeatureConfig::default().with_hooks(RuntimeHookConfig::new(
                vec![HookSpec::Command(shell_snippet(
                    "printf 'blocked by hook'; exit 2",
                ))],
                Vec::new(),
            )),
        );

        let summary = runtime
            .run_turn("use the tool", None)
            .expect("conversation should continue after hook denial");

        assert_eq!(summary.tool_results.len(), 1);
        let ContentBlock::ToolResult {
            is_error, output, ..
        } = &summary.tool_results[0].blocks[0]
        else {
            panic!("expected tool result block");
        };
        assert!(
            *is_error,
            "hook denial should produce an error result: {output}"
        );
        assert!(
            output.contains("denied tool") || output.contains("blocked by hook"),
            "unexpected hook denial output: {output:?}"
        );
    }

    #[test]
    fn appends_post_tool_hook_feedback_to_tool_result() {
        struct TwoCallApiClient {
            calls: usize,
        }

        impl ApiClient for TwoCallApiClient {
            fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
                self.calls += 1;
                match self.calls {
                    1 => Ok(vec![
                        AssistantEvent::ToolUse {
                            id: "tool-1".to_string(),
                            name: "add".to_string(),
                            input: r#"{"lhs":2,"rhs":2}"#.to_string(),
                        },
                        AssistantEvent::MessageStop,
                    ]),
                    2 => {
                        assert!(request
                            .messages
                            .iter()
                            .any(|message| message.role == MessageRole::Tool));
                        Ok(vec![
                            AssistantEvent::TextDelta("done".to_string()),
                            AssistantEvent::MessageStop,
                        ])
                    }
                    _ => Err(RuntimeError::new("unexpected extra API call")),
                }
            }
        }

        let mut runtime = ConversationRuntime::new_with_features(
            Session::new(),
            TwoCallApiClient { calls: 0 },
            StaticToolExecutor::new().register("add", |_input| Ok("4".to_string())),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec![PromptSection::new(PromptSectionKind::System, "system")],
            RuntimeFeatureConfig::default().with_hooks(RuntimeHookConfig::new(
                vec![HookSpec::Command(shell_snippet("printf 'pre hook ran'"))],
                vec![HookSpec::Command(shell_snippet("printf 'post hook ran'"))],
            )),
        );

        let summary = runtime
            .run_turn("use add", None)
            .expect("tool loop succeeds");

        assert_eq!(summary.tool_results.len(), 1);
        let ContentBlock::ToolResult {
            is_error, output, ..
        } = &summary.tool_results[0].blocks[0]
        else {
            panic!("expected tool result block");
        };
        assert!(
            !*is_error,
            "post hook should preserve non-error result: {output:?}"
        );
        assert!(
            output.contains('4'),
            "tool output missing value: {output:?}"
        );
        assert!(
            output.contains("pre hook ran"),
            "tool output missing pre hook feedback: {output:?}"
        );
        assert!(
            output.contains("post hook ran"),
            "tool output missing post hook feedback: {output:?}"
        );
    }

    #[test]
    fn reconstructs_usage_tracker_from_restored_session() {
        struct SimpleApi;
        impl ApiClient for SimpleApi {
            fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![
                    AssistantEvent::TextDelta("done".to_string()),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut session = Session::new();
        session
            .messages
            .push(crate::session::ConversationMessage::assistant_with_usage(
                vec![ContentBlock::Text {
                    text: "earlier".to_string(),
                }],
                Some(TokenUsage {
                    input_tokens: 11,
                    output_tokens: 7,
                    cache_creation_input_tokens: 2,
                    cache_read_input_tokens: 1,
                }),
            ));

        let runtime = ConversationRuntime::new(
            session,
            SimpleApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec![PromptSection::new(PromptSectionKind::System, "system")],
        );

        assert_eq!(runtime.usage().turns(), 1);
        assert_eq!(runtime.usage().cumulative_usage().total_tokens(), 21);
    }

    #[test]
    fn compacts_session_after_turns() {
        struct SimpleApi;
        impl ApiClient for SimpleApi {
            fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![
                    AssistantEvent::TextDelta("done".to_string()),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut runtime = ConversationRuntime::new(
            Session::new(),
            SimpleApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec![PromptSection::new(PromptSectionKind::System, "system")],
        );
        runtime.run_turn("a", None).expect("turn a");
        runtime.run_turn("b", None).expect("turn b");
        runtime.run_turn("c", None).expect("turn c");

        let result = runtime.compact(CompactionConfig {
            preserve_recent_messages: 2,
            max_estimated_tokens: 1, ..CompactionConfig::default()
        });
        assert!(result.summary.contains("Conversation summary"));
        assert_eq!(
            result.compacted_session.messages[0].role,
            MessageRole::System
        );
    }

    // ─── Task #561: refresh hooks for new cwd ──────────────────────────────

    /// After `refresh_hooks_from_feature_config` the runner advertises
    /// the new hook config (verified by running a SessionStart hook from
    /// the new config and seeing its stdout in the result).
    #[test]
    fn refresh_hooks_from_feature_config_swaps_in_new_runner() {
        struct NullApi;
        impl ApiClient for NullApi {
            fn stream(&mut self, _r: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![AssistantEvent::MessageStop])
            }
        }
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            NullApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec![PromptSection::new(PromptSectionKind::System, "system")],
        );
        // Before: no hooks, so the message list is empty.
        assert!(runtime.run_session_start_hooks().is_empty());

        // Build a feature config with a SessionStart command hook by
        // parsing settings JSON — the public, documented surface.
        use crate::config::hooks::parse_optional_hooks_config;
        use crate::json::JsonValue;
        let parsed = JsonValue::parse(
            r#"{"hooks":{"SessionStart":[{"type":"command","body":"printf 'fresh-runner'"}]}}"#,
        ).expect("seed JSON");
        let hook_cfg = parse_optional_hooks_config(&parsed)
            .expect("hook config parses");
        let feature_cfg = RuntimeFeatureConfig::default().with_hooks(hook_cfg);
        runtime.refresh_hooks_from_feature_config(&feature_cfg);

        // Skip on Windows: the shell snippet uses POSIX `printf`.
        #[cfg(not(windows))]
        {
            let msgs = runtime.run_session_start_hooks();
            assert!(
                msgs.iter().any(|m| m.contains("fresh-runner")),
                "refreshed runner should fire the new SessionStart hook; got {msgs:?}"
            );
        }
    }

    // ─── Task #560: per-tab autocompact threshold isolation ────────────────

    /// Tab A with high estimated tokens triggers `should_auto_compact`
    /// while Tab B at the same moment with low estimated tokens does not.
    /// Both runtimes share *no* mutable state — they each carry their own
    /// `Session` and read the threshold per-call from the caller.
    #[test]
    fn per_tab_autocompact_threshold_isolation() {
        struct NullApi;
        impl ApiClient for NullApi {
            fn stream(&mut self, _r: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![AssistantEvent::MessageStop])
            }
        }

        // Tab A: many large user turns so estimated_tokens crosses the bar.
        let mut tab_a = ConversationRuntime::new(
            Session::new(),
            NullApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec![PromptSection::new(PromptSectionKind::System, "system")],
        );
        for _ in 0..6 {
            tab_a.session.messages.push(ConversationMessage::user_text(
                "x".repeat(2_000),
            ));
        }

        // Tab B: tiny session.
        let tab_b = ConversationRuntime::new(
            Session::new(),
            NullApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec![PromptSection::new(PromptSectionKind::System, "system")],
        );

        // Threshold + context window picked so Tab A's estimate is above
        // and Tab B's is well below.
        let context_max = 8_000_usize; // small synthetic window
        let threshold_pct = 30_usize;

        assert!(
            tab_a.should_auto_compact(threshold_pct, context_max),
            "Tab A at high estimate must trigger autocompact"
        );
        assert!(
            !tab_b.should_auto_compact(threshold_pct, context_max),
            "Tab B at low estimate must NOT trigger autocompact"
        );

        // Compacting Tab A must not change Tab B's estimate (no shared state).
        let pre_b = tab_b.estimated_tokens();
        let _result = tab_a.compact(CompactionConfig {
            preserve_recent_messages: 2,
            max_estimated_tokens: 1, ..CompactionConfig::default()
        });
        let post_b = tab_b.estimated_tokens();
        assert_eq!(pre_b, post_b, "Tab B token estimate must be unaffected by Tab A's compaction");
    }

    #[cfg(windows)]
    fn shell_snippet(script: &str) -> String {
        script.replace('\'', "\"")
    }

    #[cfg(not(windows))]
    fn shell_snippet(script: &str) -> String {
        script.to_string()
    }

    // ─── Task #566: Stop hook end-to-end through ConversationRuntime ─────────

    struct OneShotApi {
        served: bool,
    }

    impl ApiClient for OneShotApi {
        fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
            self.served = true;
            Ok(vec![
                AssistantEvent::TextDelta("done".to_string()),
                AssistantEvent::Usage(TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                }),
                AssistantEvent::MessageStop,
            ])
        }
    }

    /// Counts the number of times stream() was called so a Stop-hook
    /// block can be verified by re-invocation.  Each turn the body just
    /// emits an empty assistant message + MessageStop so the loop falls
    /// through to the Stop hook every iteration.
    struct CountingApi {
        calls: usize,
        max_calls: usize,
    }

    impl ApiClient for CountingApi {
        fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
            self.calls += 1;
            if self.calls > self.max_calls {
                return Err(RuntimeError::new("CountingApi reached max_calls"));
            }
            Ok(vec![
                AssistantEvent::TextDelta(format!("turn {}", self.calls)),
                AssistantEvent::Usage(TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                }),
                AssistantEvent::MessageStop,
            ])
        }
    }

    fn runtime_with_stop_hooks<C: ApiClient, T: ToolExecutor>(
        api: C,
        tool_executor: T,
        stop_hooks: Vec<HookSpec>,
    ) -> ConversationRuntime<C, T> {
        let mut hook_config = RuntimeHookConfig::default();
        // Programmatic injection: push specs onto the runtime extras so we
        // don't need to round-trip through settings.json parsing.
        let mut features = RuntimeFeatureConfig::default();
        features = features.with_hooks(hook_config.clone());
        let mut runtime = ConversationRuntime::new_with_features(
            Session::new(),
            api,
            tool_executor,
            PermissionPolicy::new(PermissionMode::WorkspaceWrite),
            vec![PromptSection::new(PromptSectionKind::System, "system")],
            features,
        );
        // Append stop_extra via a freshly built runner.
        let mut runner = crate::hooks::HookRunner::default();
        for spec in stop_hooks {
            runner.stop_extra_mut().push(crate::hooks::RuntimeHookSpec::Plugin(spec));
        }
        runtime.set_hook_runner_for_testing(runner);
        let _ = hook_config;
        runtime
    }

    /// run_turn() invokes the Stop hook exactly once at end-of-turn when
    /// there are no pending tool_uses.
    #[test]
    fn stop_hook_invoked_at_turn_end() {
        let api = OneShotApi { served: false };
        let tool_executor = StaticToolExecutor::new();
        // Touch-witness file: hook writes to a tempfile so the test can
        // observe that the runtime really invoked it.
        let tmp = std::env::temp_dir().join(format!(
            "anvil-stop-hook-witness-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let snippet = shell_snippet(&format!(
            "printf 'invoked' > {}; printf '%s' '{{\"decision\":\"allow\"}}'",
            tmp.display()
        ));
        let mut runtime = runtime_with_stop_hooks(
            api,
            tool_executor,
            vec![HookSpec::Command(snippet)],
        );
        let summary = runtime
            .run_turn("hi", None)
            .expect("turn should complete normally");
        // Hook ran exactly once.
        assert!(
            tmp.exists(),
            "Stop hook witness file should exist after run_turn"
        );
        let body = std::fs::read_to_string(&tmp).expect("read witness");
        assert_eq!(body, "invoked");
        let _ = std::fs::remove_file(&tmp);
        // The OneShotApi only served one call → one iteration.
        assert_eq!(summary.iterations, 1);
    }

    /// A Stop hook returning `{"decision":"block","reason":"..."}` keeps
    /// the turn alive — the runtime calls the API a second time.
    #[test]
    fn stop_hook_block_decision_holds_turn() {
        let api = CountingApi {
            calls: 0,
            max_calls: 10,
        };
        let tool_executor = StaticToolExecutor::new();
        // Block exactly once then allow on the next invocation.
        let tmp = std::env::temp_dir().join(format!(
            "anvil-stop-hook-flip-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        // Shell that prints `block` on first invocation, then `allow`
        // afterwards.  Uses the witness file as a flag.
        let snippet = shell_snippet(&format!(
            "if [ -f {p} ]; then printf '%s' '{{\"decision\":\"allow\"}}'; else touch {p}; printf '%s' '{{\"decision\":\"block\",\"reason\":\"do another pass\"}}'; fi",
            p = tmp.display()
        ));
        let mut runtime = runtime_with_stop_hooks(
            api,
            tool_executor,
            vec![HookSpec::Command(snippet)],
        );
        let summary = runtime
            .run_turn("hi", None)
            .expect("turn should eventually allow stop");
        // First turn blocks → re-runs → second turn allows.
        assert!(
            summary.iterations >= 2,
            "Stop hook block must drive at least 2 iterations, got {}",
            summary.iterations
        );
        let _ = std::fs::remove_file(&tmp);
        // The injected reason should appear as a user message in the session.
        let has_reason = runtime.session().messages.iter().any(|m| {
            m.blocks.iter().any(|b| matches!(b, ContentBlock::Text { text } if text.contains("do another pass")))
        });
        assert!(has_reason, "block reason should be injected as user message");
    }

    // ─── Task #564: turn loop reactive compaction + retry ─────────────────

    /// Provider that returns ContextTooLong on the first call and a
    /// normal MessageStop on subsequent calls.  Used to verify the turn
    /// loop's reactive-compact-then-retry path lands a successful turn.
    struct OverflowOnceApi {
        calls: usize,
    }

    impl ApiClient for OverflowOnceApi {
        fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
            self.calls += 1;
            if self.calls == 1 {
                Err(RuntimeError::context_too_long(50_000))
            } else {
                Ok(vec![
                    AssistantEvent::TextDelta("compacted-and-retried".to_string()),
                    AssistantEvent::Usage(TokenUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    }),
                    AssistantEvent::MessageStop,
                ])
            }
        }
    }

    #[test]
    fn turn_loop_retries_after_reactive_compact() {
        use crate::compact::CompactionConfig;
        let api = OverflowOnceApi { calls: 0 };
        let tool_executor = StaticToolExecutor::new();
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            api,
            tool_executor,
            PermissionPolicy::new(PermissionMode::WorkspaceWrite),
            vec![PromptSection::new(PromptSectionKind::System, "system")],
        );
        // Enable reactive compaction with one retry (default).
        runtime.set_compaction_config(CompactionConfig {
            reactive_enabled: true,
            reactive_max_retries: 1,
            ..CompactionConfig::default()
        });
        let summary = runtime
            .run_turn("hi", None)
            .expect("turn should succeed after reactive compact + retry");
        assert!(
            summary.iterations >= 1,
            "expected at least one successful iteration after retry"
        );
        // The compacted assistant message should be the final one.
        let last_text = runtime
            .session()
            .messages
            .last()
            .and_then(|m| {
                m.blocks.iter().find_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
            })
            .unwrap_or_default();
        assert!(
            last_text.contains("compacted-and-retried"),
            "expected post-retry text, got: {last_text:?}"
        );
    }

    /// When reactive_enabled is false, ContextTooLong surfaces verbatim.
    #[test]
    fn turn_loop_does_not_retry_when_reactive_disabled() {
        use crate::compact::CompactionConfig;
        let api = OverflowOnceApi { calls: 0 };
        let tool_executor = StaticToolExecutor::new();
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            api,
            tool_executor,
            PermissionPolicy::new(PermissionMode::WorkspaceWrite),
            vec![PromptSection::new(PromptSectionKind::System, "system")],
        );
        runtime.set_compaction_config(CompactionConfig {
            reactive_enabled: false,
            ..CompactionConfig::default()
        });
        let err = runtime
            .run_turn("hi", None)
            .expect_err("should propagate ContextTooLong when reactive disabled");
        assert!(
            err.context_too_long_overflow().is_some(),
            "error should carry context-too-long overflow"
        );
    }

    /// When a Stop hook blocks more times than the configured cap, the
    /// runtime force-stops the loop so a buggy hook can't spin forever.
    #[test]
    fn stop_hook_block_count_cap_force_stops() {
        let api = CountingApi {
            calls: 0,
            max_calls: 20,
        };
        let tool_executor = StaticToolExecutor::new();
        // A hook that ALWAYS blocks.
        let snippet = shell_snippet(
            r#"printf '%s' '{"decision":"block","reason":"always block"}'"#,
        );
        let mut runtime = runtime_with_stop_hooks(
            api,
            tool_executor,
            vec![HookSpec::Command(snippet)],
        );
        // Lower the cap so the test is fast.
        runtime.set_stop_hook_cap_for_testing(2);
        let summary = runtime
            .run_turn("hi", None)
            .expect("turn should force-stop once cap is hit");
        assert!(
            summary.iterations <= 3,
            "force-stop should land in <= 3 iterations (cap=2), got {}",
            summary.iterations
        );
        assert!(
            runtime.stop_hook_counter().blocks_seen() >= 2,
            "block counter should have recorded the blocks"
        );
    }
}
