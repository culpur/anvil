//! Provider client setup, runtime construction, and tool execution infrastructure.
//!
//! Extracted from main.rs. Contains:
//!   - Plugin manager setup
//!   - Runtime builder functions
//!   - DefaultRuntimeClient (ApiClient impl, streaming)
//!   - CliToolExecutor (ToolExecutor impl, MCP/LSP/Agent routing)
//!   - InternalPromptProgress types
//!   - Utility functions: final_assistant_text, collect_tool_uses/results
//!     slash_command_completion_candidates, suggest_repl_commands, edit_distance,
//!     describe_tool_progress, permission_policy, convert_messages

#[allow(unused_imports)]
use std::collections::BTreeSet;
use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use api::{
    AnvilApiClient, AuthSource, ContentBlockDelta, ImageSource, ImageSourceKind,
    InputContentBlock, InputMessage, MessageRequest, OutputContentBlock, ProviderClient,
    ProviderKind, StreamEvent as ApiStreamEvent, ToolChoice, ToolResultContentBlock,
    detect_provider_kind, max_tokens_for_model, resolve_startup_auth_source,
};
use commands::slash_command_specs;
use crate::format_tool::{
    extract_tool_path, first_visible_line, push_output_block, response_to_events,
    summarize_tool_payload, tool_call_detail, truncate_for_summary,
};
use plugins::{PluginManager, PluginManagerConfig};
use crate::render::{
    BlockState, MarkdownStreamState, TerminalRenderer,
    render_permission_prompt, render_tool_call_block, render_tool_result_block,
};
use runtime::{
    ApiClient, ApiRequest, AssistantEvent, ConfigLoader, ContentBlock,
    ConversationMessage, ConversationRuntime, LspManager, LspServerConfig,
    McpServerManager, MessageRole, PermissionMode, PermissionPolicy, RuntimeError,
    Session, TokenUsage, ToolError, ToolExecutor,
};
use serde_json::json;
use tools::{GlobalToolRegistry, McpToolDefinition};
use crate::tui::{TuiEvent, TuiSender};

use crate::{
    AllowedToolSet, TuiSenderSlot, INTERNAL_PROGRESS_HEARTBEAT_INTERVAL,
    filter_tool_specs,
    agents,
};

fn build_runtime_plugin_state(
) -> Result<(runtime::RuntimeFeatureConfig, GlobalToolRegistry, runtime::RuntimeConfig), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    let runtime_config = loader.load()?;
    let plugin_manager = build_plugin_manager(&cwd, &loader, &runtime_config);
    let tool_registry = GlobalToolRegistry::with_plugin_tools(plugin_manager.aggregated_tools()?)?;
    Ok((runtime_config.feature_config().clone(), tool_registry, runtime_config))
}

pub(crate) fn build_plugin_manager(
    cwd: &Path,
    loader: &ConfigLoader,
    runtime_config: &runtime::RuntimeConfig,
) -> PluginManager {
    let plugin_settings = runtime_config.plugins();
    let mut plugin_config = PluginManagerConfig::new(loader.config_home().to_path_buf());
    plugin_config.enabled_plugins = plugin_settings.enabled_plugins().clone();
    plugin_config.external_dirs = plugin_settings
        .external_directories()
        .iter()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path))
        .collect();
    plugin_config.install_root = plugin_settings
        .install_root()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path));
    plugin_config.registry_path = plugin_settings
        .registry_path()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path));
    plugin_config.bundled_root = plugin_settings
        .bundled_root()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path));
    PluginManager::new(plugin_config)
}

fn resolve_plugin_path(cwd: &Path, config_home: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else if value.starts_with('.') {
        cwd.join(path)
    } else {
        config_home.join(path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InternalPromptProgressState {
    pub(crate) command_label: &'static str,
    pub(crate) task_label: String,
    pub(crate) step: usize,
    pub(crate) phase: String,
    pub(crate) detail: Option<String>,
    pub(crate) saw_final_text: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InternalPromptProgressEvent {
    Started,
    Update,
    Heartbeat,
    Complete,
    Failed,
}

#[derive(Debug)]
struct InternalPromptProgressShared {
    state: Mutex<InternalPromptProgressState>,
    output_lock: Mutex<()>,
    started_at: Instant,
}

#[derive(Debug, Clone)]
pub(crate) struct InternalPromptProgressReporter {
    shared: Arc<InternalPromptProgressShared>,
}

#[derive(Debug)]
pub(crate) struct InternalPromptProgressRun {
    reporter: InternalPromptProgressReporter,
    heartbeat_stop: Option<mpsc::Sender<()>>,
    heartbeat_handle: Option<thread::JoinHandle<()>>,
}

impl InternalPromptProgressReporter {
    fn ultraplan(task: &str) -> Self {
        Self {
            shared: Arc::new(InternalPromptProgressShared {
                state: Mutex::new(InternalPromptProgressState {
                    command_label: "Ultraplan",
                    task_label: task.to_string(),
                    step: 0,
                    phase: "planning started".to_string(),
                    detail: Some(format!("task: {task}")),
                    saw_final_text: false,
                }),
                output_lock: Mutex::new(()),
                started_at: Instant::now(),
            }),
        }
    }

    fn emit(&self, event: InternalPromptProgressEvent, error: Option<&str>) {
        let snapshot = self.snapshot();
        let line = format_internal_prompt_progress_line(event, &snapshot, self.elapsed(), error);
        self.write_line(&line);
    }

    fn mark_model_phase(&self) {
        let snapshot = {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("internal prompt progress state poisoned");
            state.step += 1;
            state.phase = if state.step == 1 {
                "analyzing request".to_string()
            } else {
                "reviewing findings".to_string()
            };
            state.detail = Some(format!("task: {}", state.task_label));
            state.clone()
        };
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Update,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn mark_tool_phase(&self, name: &str, input: &str) {
        let detail = describe_tool_progress(name, input);
        let snapshot = {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("internal prompt progress state poisoned");
            state.step += 1;
            state.phase = format!("running {name}");
            state.detail = Some(detail);
            state.clone()
        };
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Update,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn mark_text_phase(&self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let detail = truncate_for_summary(first_visible_line(trimmed), 120);
        let snapshot = {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("internal prompt progress state poisoned");
            if state.saw_final_text {
                return;
            }
            state.saw_final_text = true;
            state.step += 1;
            state.phase = "drafting final plan".to_string();
            state.detail = (!detail.is_empty()).then_some(detail);
            state.clone()
        };
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Update,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn emit_heartbeat(&self) {
        let snapshot = self.snapshot();
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Heartbeat,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn snapshot(&self) -> InternalPromptProgressState {
        self.shared
            .state
            .lock()
            .expect("internal prompt progress state poisoned")
            .clone()
    }

    fn elapsed(&self) -> Duration {
        self.shared.started_at.elapsed()
    }

    fn write_line(&self, line: &str) {
        let _guard = self
            .shared
            .output_lock
            .lock()
            .expect("internal prompt progress output lock poisoned");
        let mut stdout = io::stdout();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }
}

impl InternalPromptProgressRun {
    pub(crate) fn start_ultraplan(task: &str) -> Self {
        let reporter = InternalPromptProgressReporter::ultraplan(task);
        reporter.emit(InternalPromptProgressEvent::Started, None);

        let (heartbeat_stop, heartbeat_rx) = mpsc::channel();
        let heartbeat_reporter = reporter.clone();
        let heartbeat_handle = thread::spawn(move || loop {
            match heartbeat_rx.recv_timeout(INTERNAL_PROGRESS_HEARTBEAT_INTERVAL) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => heartbeat_reporter.emit_heartbeat(),
            }
        });

        Self {
            reporter,
            heartbeat_stop: Some(heartbeat_stop),
            heartbeat_handle: Some(heartbeat_handle),
        }
    }

    pub(crate) fn reporter(&self) -> InternalPromptProgressReporter {
        self.reporter.clone()
    }

    pub(crate) fn finish_success(&mut self) {
        self.stop_heartbeat();
        self.reporter
            .emit(InternalPromptProgressEvent::Complete, None);
    }

    pub(crate) fn finish_failure(&mut self, error: &str) {
        self.stop_heartbeat();
        self.reporter
            .emit(InternalPromptProgressEvent::Failed, Some(error));
    }

    fn stop_heartbeat(&mut self) {
        if let Some(sender) = self.heartbeat_stop.take() {
            let _ = sender.send(());
        }
        if let Some(handle) = self.heartbeat_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for InternalPromptProgressRun {
    fn drop(&mut self) {
        self.stop_heartbeat();
    }
}

pub(crate) fn format_internal_prompt_progress_line(
    event: InternalPromptProgressEvent,
    snapshot: &InternalPromptProgressState,
    elapsed: Duration,
    error: Option<&str>,
) -> String {
    let elapsed_seconds = elapsed.as_secs();
    let step_label = if snapshot.step == 0 {
        "current step pending".to_string()
    } else {
        format!("current step {}", snapshot.step)
    };
    let mut status_bits = vec![step_label, format!("phase {}", snapshot.phase)];
    if let Some(detail) = snapshot
        .detail
        .as_deref()
        .filter(|detail| !detail.is_empty())
    {
        status_bits.push(detail.to_string());
    }
    let status = status_bits.join(" · ");
    match event {
        InternalPromptProgressEvent::Started => {
            format!(
                "🧭 {} status · planning started · {status}",
                snapshot.command_label
            )
        }
        InternalPromptProgressEvent::Update => {
            format!("… {} status · {status}", snapshot.command_label)
        }
        InternalPromptProgressEvent::Heartbeat => format!(
            "… {} heartbeat · {elapsed_seconds}s elapsed · {status}",
            snapshot.command_label
        ),
        InternalPromptProgressEvent::Complete => format!(
            "✔ {} status · completed · {elapsed_seconds}s elapsed · {} steps total",
            snapshot.command_label, snapshot.step
        ),
        InternalPromptProgressEvent::Failed => format!(
            "✘ {} status · failed · {elapsed_seconds}s elapsed · {}",
            snapshot.command_label,
            error.unwrap_or("unknown error")
        ),
    }
}

pub(crate) fn describe_tool_progress(name: &str, input: &str) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(input).unwrap_or(serde_json::Value::String(input.to_string()));
    match name {
        "bash" | "Bash" => {
            let command = parsed
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if command.is_empty() {
                "running shell command".to_string()
            } else {
                format!("command {}", truncate_for_summary(command.trim(), 100))
            }
        }
        "read_file" | "Read" => format!("reading {}", extract_tool_path(&parsed)),
        "write_file" | "Write" => format!("writing {}", extract_tool_path(&parsed)),
        "edit_file" | "Edit" => format!("editing {}", extract_tool_path(&parsed)),
        "glob_search" | "Glob" => {
            let pattern = parsed
                .get("pattern")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let scope = parsed
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or(".");
            format!("glob `{pattern}` in {scope}")
        }
        "grep_search" | "Grep" => {
            let pattern = parsed
                .get("pattern")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let scope = parsed
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or(".");
            format!("grep `{pattern}` in {scope}")
        }
        "web_search" | "WebSearch" => parsed
            .get("query")
            .and_then(|value| value.as_str())
            .map_or_else(
                || "running web search".to_string(),
                |query| format!("query {}", truncate_for_summary(query, 100)),
            ),
        _ => {
            let summary = summarize_tool_payload(input);
            if summary.is_empty() {
                format!("running {name}")
            } else {
                format!("{name}: {summary}")
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_runtime(
    session: Session,
    model: String,
    system_prompt: Vec<String>,
    enable_tools: bool,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
    progress_reporter: Option<InternalPromptProgressReporter>,
) -> Result<ConversationRuntime<DefaultRuntimeClient, CliToolExecutor>, Box<dyn std::error::Error>>
{
    // A blank slot: no TUI by default.  Callers that want TUI output call
    // build_runtime_with_tui_slot() instead.
    let slot: TuiSenderSlot = Arc::new(Mutex::new(None));
    // Standalone agent manager for non-TUI callers.
    let agent_manager: Arc<Mutex<agents::AgentManager>> =
        Arc::new(Mutex::new(agents::AgentManager::new()));
    build_runtime_with_tui_slot(
        session, model, system_prompt, enable_tools, emit_output,
        allowed_tools, permission_mode, progress_reporter, slot, agent_manager,
    )
}

#[allow(clippy::needless_pass_by_value)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_runtime_with_tui_slot(
    session: Session,
    model: String,
    system_prompt: Vec<String>,
    enable_tools: bool,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
    progress_reporter: Option<InternalPromptProgressReporter>,
    tui_slot: TuiSenderSlot,
    agent_manager: Arc<Mutex<agents::AgentManager>>,
) -> Result<ConversationRuntime<DefaultRuntimeClient, CliToolExecutor>, Box<dyn std::error::Error>>
{
    let (feature_config, mut tool_registry, runtime_config) = build_runtime_plugin_state()?;

    // Build and initialize the MCP server manager, then inject discovered tools
    let mcp_manager = {
        let mut manager = McpServerManager::from_runtime_config(&runtime_config);
        let tokio_rt = tokio::runtime::Runtime::new()?;
        let discovered = tokio_rt.block_on(manager.discover_tools()).unwrap_or_else(|err| {
            eprintln!("[mcp] tool discovery failed: {err}");
            Vec::new()
        });
        let mcp_defs = discovered
            .into_iter()
            .map(|t| McpToolDefinition {
                name: t.qualified_name,
                description: t.tool.description,
                input_schema: t.tool.input_schema,
            })
            .collect::<Vec<_>>();
        tool_registry.add_mcp_tools(mcp_defs);
        Arc::new(Mutex::new(manager))
    };

    // Build the LSP manager from config entries, if any are configured.
    let lsp_manager = {
        let lsp_cfg = runtime_config.lsp();
        let manager = if lsp_cfg.servers.is_empty() {
            None
        } else {
            let server_configs = lsp_cfg
                .servers
                .iter()
                .map(|entry| LspServerConfig {
                    name: entry.name.clone(),
                    command: entry.command.clone(),
                    args: entry.args.clone(),
                    env: entry.env.clone(),
                    workspace_root: entry.workspace_root.clone(),
                    initialization_options: None,
                    extension_to_language: entry.extension_to_language.clone(),
                })
                .collect::<Vec<_>>();
            match LspManager::new(server_configs) {
                Ok(m) => Some(m),
                Err(err) => {
                    eprintln!("[lsp] failed to initialize LSP manager: {err}");
                    None
                }
            }
        };
        Arc::new(Mutex::new(manager))
    };

    Ok(ConversationRuntime::new_with_features(
        session,
        DefaultRuntimeClient::new(
            model.clone(),
            enable_tools,
            emit_output,
            allowed_tools.clone(),
            tool_registry.clone(),
            progress_reporter,
            tui_slot.clone(),
        )?,
        CliToolExecutor::new(
            allowed_tools.clone(),
            emit_output,
            tool_registry.clone(),
            mcp_manager,
            lsp_manager,
            tui_slot,
            agent_manager,
            model,
        ),
        permission_policy(permission_mode, &tool_registry),
        system_prompt,
        feature_config,
    ))
}

pub(crate) struct CliPermissionPrompter {
    current_mode: PermissionMode,
}

impl CliPermissionPrompter {
    pub(crate) fn new(current_mode: PermissionMode) -> Self {
        Self { current_mode }
    }
}

impl runtime::PermissionPrompter for CliPermissionPrompter {
    fn decide(
        &mut self,
        request: &runtime::PermissionRequest,
    ) -> runtime::PermissionPromptDecision {
        // Truncate long inputs to keep the box readable
        let input_summary = if request.input.len() > 160 {
            format!("{}…", &request.input[..160])
        } else {
            request.input.clone()
        };

        let mut stdout = io::stdout();
        let mut stdin = io::BufReader::new(io::stdin());
        let response = render_permission_prompt(
            &request.tool_name,
            self.current_mode.as_str(),
            request.required_mode.as_str(),
            &input_summary,
            &mut stdout,
            &mut stdin,
        );

        match response {
            Ok(line) => {
                let normalized = line.trim().to_ascii_lowercase();
                match normalized.as_str() {
                    "y" | "yes" | "always" => runtime::PermissionPromptDecision::Allow,
                    _ => runtime::PermissionPromptDecision::Deny {
                        reason: format!(
                            "tool '{}' denied by user approval prompt",
                            request.tool_name
                        ),
                    },
                }
            }
            Err(error) => runtime::PermissionPromptDecision::Deny {
                reason: format!("permission approval failed: {error}"),
            },
        }
    }
}

pub(crate) struct DefaultRuntimeClient {
    runtime: tokio::runtime::Runtime,
    client: ProviderClient,
    model: String,
    enable_tools: bool,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    tool_registry: GlobalToolRegistry,
    progress_reporter: Option<InternalPromptProgressReporter>,
    /// Shared slot — when the inner value is `Some`, stream output goes to the
    /// TUI instead of stdout.
    tui_slot: TuiSenderSlot,
}

impl DefaultRuntimeClient {
    fn new(
        model: String,
        enable_tools: bool,
        emit_output: bool,
        allowed_tools: Option<AllowedToolSet>,
        tool_registry: GlobalToolRegistry,
        progress_reporter: Option<InternalPromptProgressReporter>,
        tui_slot: TuiSenderSlot,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let client = build_provider_client(&model)?;
        Ok(Self {
            runtime: tokio::runtime::Runtime::new()?,
            client,
            model,
            enable_tools,
            emit_output,
            allowed_tools,
            tool_registry,
            progress_reporter,
            tui_slot,
        })
    }
}

/// Build the correct `ProviderClient` for the given model name.
///
/// For Anthropic models the existing OAuth / API-key resolution path is used
/// so that saved credentials continue to work.  For other providers the
/// environment-based resolution in `ProviderClient::from_model` handles it.
pub(crate) fn build_provider_client(model: &str) -> Result<ProviderClient, Box<dyn std::error::Error>> {
    let kind = detect_provider_kind(model);
    match kind {
        ProviderKind::AnvilApi => {
            let auth = resolve_cli_auth_source()?;
            Ok(ProviderClient::AnvilApi(
                AnvilApiClient::from_auth(auth).with_base_url(api::read_base_url()),
            ))
        }
        _ => Ok(ProviderClient::from_model(model)?),
    }
}

pub(crate) fn resolve_cli_auth_source() -> Result<AuthSource, Box<dyn std::error::Error>> {
    Ok(resolve_startup_auth_source(|| {
        let cwd = env::current_dir().map_err(api::ApiError::from)?;
        let config = ConfigLoader::default_for(&cwd).load().map_err(|error| {
            api::ApiError::Auth(format!("failed to load runtime OAuth config: {error}"))
        })?;
        Ok(config.oauth().cloned())
    })?)
}

impl ApiClient for DefaultRuntimeClient {
    #[allow(clippy::too_many_lines)]
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        if let Some(progress_reporter) = &self.progress_reporter {
            progress_reporter.mark_model_phase();
        }
        let message_request = MessageRequest {
            model: self.model.clone(),
            max_tokens: max_tokens_for_model(&self.model),
            messages: convert_messages(&request.messages),
            system: (!request.system_prompt.is_empty()).then(|| request.system_prompt.join("\n\n")),
            tools: self
                .enable_tools
                .then(|| filter_tool_specs(&self.tool_registry, self.allowed_tools.as_ref())),
            tool_choice: self.enable_tools.then_some(ToolChoice::Auto),
            stream: true,
        };

        // Snapshot the TUI sender (if any) before entering the async block.
        let tui_tx: Option<TuiSender> = self
            .tui_slot
            .lock()
            .ok()
            .and_then(|guard| guard.clone());

        self.runtime.block_on(async {
            let mut stream = self
                .client
                .stream_message(&message_request)
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))?;
            let mut stdout = io::stdout();
            let mut sink = io::sink();
            // When a TUI sender is active we always use sink for stdout (output
            // goes via TuiEvents instead).
            let out: &mut dyn Write = if self.emit_output && tui_tx.is_none() {
                &mut stdout
            } else {
                &mut sink
            };
            let renderer = TerminalRenderer::new();
            let mut markdown_stream = MarkdownStreamState::default();
            let mut events = Vec::new();
            let mut pending_tool: Option<(String, String, String)> = None;
            let mut saw_stop = false;

            while let Some(event) = stream
                .next_event()
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))?
            {
                match event {
                    ApiStreamEvent::MessageStart(start) => {
                        for block in start.message.content {
                            push_output_block(block, out, &mut events, &mut pending_tool, true)?;
                        }
                    }
                    ApiStreamEvent::ContentBlockStart(start) => {
                        push_output_block(
                            start.content_block,
                            out,
                            &mut events,
                            &mut pending_tool,
                            true,
                        )?;
                    }
                    ApiStreamEvent::ContentBlockDelta(delta) => match delta.delta {
                        ContentBlockDelta::TextDelta { text } => {
                            if !text.is_empty() {
                                if let Some(progress_reporter) = &self.progress_reporter {
                                    progress_reporter.mark_text_phase(&text);
                                }
                                if let Some(ref tx) = tui_tx {
                                    // Route text delta to TUI
                                    tx.send(TuiEvent::TextDelta(text.clone()));
                                } else if let Some(rendered) = markdown_stream.push(&renderer, &text) {
                                    write!(out, "{rendered}")
                                        .and_then(|()| out.flush())
                                        .map_err(|error| RuntimeError::new(error.to_string()))?;
                                }
                                events.push(AssistantEvent::TextDelta(text));
                            }
                        }
                        ContentBlockDelta::InputJsonDelta { partial_json } => {
                            if let Some((_, _, input)) = &mut pending_tool {
                                input.push_str(&partial_json);
                            }
                        }
                        ContentBlockDelta::ThinkingDelta { .. }
                        | ContentBlockDelta::SignatureDelta { .. } => {}
                    },
                    ApiStreamEvent::ContentBlockStop(_) => {
                        if let Some(ref tx) = tui_tx {
                            // Signal end of this text block
                            tx.send(TuiEvent::TextDone);
                        } else if let Some(rendered) = markdown_stream.flush(&renderer) {
                            write!(out, "{rendered}")
                                .and_then(|()| out.flush())
                                .map_err(|error| RuntimeError::new(error.to_string()))?;
                        }
                        if let Some((id, name, input)) = pending_tool.take() {
                            if let Some(progress_reporter) = &self.progress_reporter {
                                progress_reporter.mark_tool_phase(&name, &input);
                            }
                            let detail = tool_call_detail(&name, &input);
                            if let Some(ref tx) = tui_tx {
                                // Send tool call to TUI
                                tx.send(TuiEvent::ToolCallActive {
                                    name: name.clone(),
                                    detail,
                                });
                            } else {
                                // Display tool call block now that input is fully accumulated
                                writeln!(out).map_err(|error| RuntimeError::new(error.to_string()))?;
                                render_tool_call_block(&name, &detail, BlockState::Active, out)
                                    .map_err(|error| RuntimeError::new(error.to_string()))?;
                            }
                            events.push(AssistantEvent::ToolUse { id, name, input });
                        }
                    }
                    ApiStreamEvent::MessageDelta(delta) => {
                        if let Some(ref tx) = tui_tx {
                            tx.send(TuiEvent::Tokens {
                                input: delta.usage.input_tokens,
                                output: delta.usage.output_tokens,
                            });
                        }
                        events.push(AssistantEvent::Usage(TokenUsage {
                            input_tokens: delta.usage.input_tokens,
                            output_tokens: delta.usage.output_tokens,
                            cache_creation_input_tokens: 0,
                            cache_read_input_tokens: 0,
                        }));
                    }
                    ApiStreamEvent::MessageStop(_) => {
                        saw_stop = true;
                        if let Some(ref tx) = tui_tx {
                            tx.send(TuiEvent::TextDone);
                        } else if let Some(rendered) = markdown_stream.flush(&renderer) {
                            write!(out, "{rendered}")
                                .and_then(|()| out.flush())
                                .map_err(|error| RuntimeError::new(error.to_string()))?;
                        }
                        events.push(AssistantEvent::MessageStop);
                    }
                }
            }

            if !saw_stop
                && events.iter().any(|event| {
                    matches!(event, AssistantEvent::TextDelta(text) if !text.is_empty())
                        || matches!(event, AssistantEvent::ToolUse { .. })
                })
            {
                events.push(AssistantEvent::MessageStop);
            }

            if events
                .iter()
                .any(|event| matches!(event, AssistantEvent::MessageStop))
            {
                return Ok(events);
            }

            let response = self
                .client
                .send_message(&MessageRequest {
                    stream: false,
                    ..message_request.clone()
                })
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))?;
            response_to_events(response, out)
        })
    }
}

pub(crate) fn final_assistant_text(summary: &runtime::TurnSummary) -> String {
    summary
        .assistant_messages
        .last()
        .map(|message| {
            message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

pub(crate) fn collect_tool_uses(summary: &runtime::TurnSummary) -> Vec<serde_json::Value> {
    summary
        .assistant_messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some(json!({
                "id": id,
                "name": name,
                "input": input,
            })),
            _ => None,
        })
        .collect()
}

pub(crate) fn collect_tool_results(summary: &runtime::TurnSummary) -> Vec<serde_json::Value> {
    summary
        .tool_results
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                tool_name,
                output,
                is_error,
            } => Some(json!({
                "tool_use_id": tool_use_id,
                "tool_name": tool_name,
                "output": output,
                "is_error": is_error,
            })),
            _ => None,
        })
        .collect()
}

pub(crate) fn slash_command_completion_candidates() -> Vec<String> {
    let mut candidates = slash_command_specs()
        .iter()
        .flat_map(|spec| {
            std::iter::once(spec.name)
                .chain(spec.aliases.iter().copied())
                .map(|name| format!("/{name}"))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    candidates.extend([
        String::from("/vim"),
        String::from("/exit"),
        String::from("/quit"),
        // /configure sub-command completions
        String::from("/configure providers"),
        String::from("/configure models"),
        String::from("/configure context"),
        String::from("/configure search"),
        String::from("/configure permissions"),
        String::from("/configure display"),
        String::from("/configure integrations"),
    ]);
    candidates.sort();
    candidates.dedup();
    candidates
}

pub(crate) fn suggest_repl_commands(name: &str) -> Vec<String> {
    let normalized = name.trim().trim_start_matches('/').to_ascii_lowercase();
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut ranked = slash_command_completion_candidates()
        .into_iter()
        .filter_map(|candidate| {
            let raw = candidate.trim_start_matches('/').to_ascii_lowercase();
            let distance = edit_distance(&normalized, &raw);
            let prefix_match = raw.starts_with(&normalized) || normalized.starts_with(&raw);
            let near_match = distance <= 2;
            (prefix_match || near_match).then_some((distance, candidate))
        })
        .collect::<Vec<_>>();
    ranked.sort();
    ranked.dedup_by(|left, right| left.1 == right.1);
    ranked
        .into_iter()
        .map(|(_, candidate)| candidate)
        .take(3)
        .collect()
}

fn edit_distance(left: &str, right: &str) -> usize {
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
            let substitution_cost = usize::from(left_char != *right_char);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + substitution_cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[right_chars.len()]
}

/// Extract a human-readable detail string from a tool call input JSON for use
/// inside `render_tool_call_block`. Returns plain text (no ANSI escape codes).
pub(crate) struct CliToolExecutor {
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    tool_registry: GlobalToolRegistry,
    mcp_manager: Arc<Mutex<McpServerManager>>,
    lsp_manager: Arc<Mutex<Option<LspManager>>>,
    tokio_rt: tokio::runtime::Runtime,
    /// Shared slot — when the inner value is `Some`, tool output goes to the
    /// TUI instead of stdout.
    tui_slot: TuiSenderSlot,
    /// Shared agent manager — used to register real agent threads spawned by
    /// the `Agent` and `TeamDelegate` tool calls, keeping the TUI panel in sync.
    agent_manager: Arc<Mutex<agents::AgentManager>>,
    /// Model name used to build provider clients inside agent threads.
    model: String,
}

impl CliToolExecutor {
    fn new(
        allowed_tools: Option<AllowedToolSet>,
        emit_output: bool,
        tool_registry: GlobalToolRegistry,
        mcp_manager: Arc<Mutex<McpServerManager>>,
        lsp_manager: Arc<Mutex<Option<LspManager>>>,
        tui_slot: TuiSenderSlot,
        agent_manager: Arc<Mutex<agents::AgentManager>>,
        model: String,
    ) -> Self {
        Self {
            emit_output,
            allowed_tools,
            tool_registry,
            mcp_manager,
            lsp_manager,
            tokio_rt: tokio::runtime::Runtime::new()
                .expect("failed to create tokio runtime for CliToolExecutor"),
            tui_slot,
            agent_manager,
            model,
        }
    }
}

impl ToolExecutor for CliToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        if self
            .allowed_tools
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(tool_name))
        {
            return Err(ToolError::new(format!(
                "tool `{tool_name}` is not enabled by the current --allowedTools setting"
            )));
        }
        let value: serde_json::Value = serde_json::from_str(input)
            .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;

        if tool_name == "AskUserQuestion" {
            let question = value
                .get("question")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::new("AskUserQuestion: missing required field `question`"))?;
            let options: Option<Vec<&str>> = value
                .get("options")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|s| s.as_str()).collect());

            let mut stdout = io::stdout();
            // Print the question with a distinct visual treatment.
            let _ = writeln!(stdout, "\x1b[1;36m?\x1b[0m \x1b[1m{question}\x1b[0m");
            if let Some(ref choices) = options {
                for (i, choice) in choices.iter().enumerate() {
                    let _ = writeln!(stdout, "  \x1b[38;5;245m{num}.\x1b[0m {choice}", num = i + 1);
                }
                let _ = write!(stdout, "\x1b[38;5;245mEnter number or text:\x1b[0m ");
            } else {
                let _ = write!(stdout, "\x1b[38;5;245mYour answer:\x1b[0m ");
            }
            let _ = stdout.flush();

            let mut line = String::new();
            if io::stdin().is_terminal() {
                io::stdin()
                    .read_line(&mut line)
                    .map_err(|e| ToolError::new(format!("AskUserQuestion: failed to read input: {e}")))?;
            } else {
                // Non-TTY (piped input): read one line if available, otherwise fall back gracefully.
                match io::stdin().read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        line = String::from("<no input — stdin is not a terminal>");
                    }
                    Ok(_) => {}
                }
            }
            let response = line.trim().to_string();

            // If options were provided, attempt to resolve a numeric selection.
            let resolved = if let Some(ref choices) = options {
                response
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| {
                        if n >= 1 && n <= choices.len() {
                            Some(choices[n - 1].to_string())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| response.clone())
            } else {
                response.clone()
            };

            if self.emit_output {
                let summary = format!("User answered: {resolved}");
                render_tool_result_block(tool_name, &summary, false, &mut io::stdout())
                    .map_err(|e| ToolError::new(e.to_string()))?;
            }
            return Ok(resolved);
        }

        // Route MCP tool calls (mcp__server__tool pattern)
        if tool_name.starts_with("mcp__") {
            let result = self.execute_mcp_tool(tool_name, &value);
            return self.emit_and_return(tool_name, result);
        }

        // Route MCP resource management built-ins
        if tool_name == "ListMcpResourcesTool" || tool_name == "ReadMcpResourceTool" {
            let result = self.execute_mcp_resource_tool(tool_name, &value);
            return self.emit_and_return(tool_name, result);
        }

        // Route LSP tool calls
        if tool_name == "LSPTool" {
            let result = self.execute_lsp_tool(&value);
            return self.emit_and_return(tool_name, result);
        }

        // ── Agent tool — intercept to wire TUI panel ───────────────────────
        // The builtin execute_agent already spawns a real thread that runs a
        // full sub-agent LLM conversation (via ProviderRuntimeClient).  We
        // additionally register a mirror AgentHandle in the AgentManager so
        // the TUI panel shows the agent as Running and updates when it finishes.
        if tool_name == "Agent" {
            let result = self.tool_registry.execute(tool_name, &value);
            if let Ok(ref json_str) = result {
                self.register_agent_in_manager(json_str, &value);
            }
            return self.emit_and_return(tool_name, result);
        }

        // ── TeamDelegate — spawn a real LLM worker via AgentManager ─────────
        // The builtin delegate_task records a shell-task stub.  We additionally
        // spawn a proper LLM thread so the team member actually generates output.
        if tool_name == "TeamDelegate" {
            let result = self.tool_registry.execute(tool_name, &value);
            if result.is_ok() {
                self.spawn_team_delegate_agent(&value);
            }
            return self.emit_and_return(tool_name, result);
        }

        let result = self.tool_registry.execute(tool_name, &value);
        self.emit_and_return(tool_name, result)
    }
}

impl CliToolExecutor {
    fn emit_and_return(
        &mut self,
        tool_name: &str,
        result: Result<String, String>,
    ) -> Result<String, ToolError> {
        let tui_tx: Option<TuiSender> = self
            .tui_slot
            .lock()
            .ok()
            .and_then(|guard| guard.clone());

        match result {
            Ok(output) => {
                if let Some(ref tx) = tui_tx {
                    tx.send(TuiEvent::ToolResult {
                        name: tool_name.to_string(),
                        summary: output.clone(),
                        is_error: false,
                    });
                } else if self.emit_output {
                    render_tool_result_block(tool_name, &output, false, &mut io::stdout())
                        .map_err(|error| ToolError::new(error.to_string()))?;
                }
                Ok(output)
            }
            Err(error) => {
                if let Some(ref tx) = tui_tx {
                    tx.send(TuiEvent::ToolResult {
                        name: tool_name.to_string(),
                        summary: error.clone(),
                        is_error: true,
                    });
                } else if self.emit_output {
                    render_tool_result_block(tool_name, &error, true, &mut io::stdout())
                        .map_err(|stream_error| ToolError::new(stream_error.to_string()))?;
                }
                Err(ToolError::new(error))
            }
        }
    }

    fn execute_mcp_tool(
        &mut self,
        qualified_name: &str,
        input: &serde_json::Value,
    ) -> Result<String, String> {
        let args = if input.is_null()
            || input
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        {
            None
        } else {
            Some(input.clone())
        };

        let mut manager = self
            .mcp_manager
            .lock()
            .map_err(|e| format!("MCP manager lock poisoned: {e}"))?;
        let response = self
            .tokio_rt
            .block_on(manager.call_tool(qualified_name, args))
            .map_err(|e| e.to_string())?;

        if let Some(err) = response.error {
            return Err(format!("MCP error {}: {}", err.code, err.message));
        }

        let result = response
            .result
            .ok_or_else(|| "MCP call returned no result".to_string())?;

        if result.is_error == Some(true) {
            let text = result
                .content
                .iter()
                .filter_map(|c| {
                    c.data
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
                .join("\n");
            return Err(if text.is_empty() {
                "MCP tool returned error".to_string()
            } else {
                text
            });
        }

        let text = result
            .content
            .iter()
            .filter_map(|c| {
                if c.kind == "text" {
                    c.data
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                } else {
                    serde_json::to_string(&c.data).ok()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(if text.is_empty() {
            serde_json::to_string_pretty(
                &result
                    .structured_content
                    .unwrap_or(serde_json::Value::Null),
            )
            .unwrap_or_default()
        } else {
            text
        })
    }

    fn execute_mcp_resource_tool(
        &mut self,
        tool_name: &str,
        value: &serde_json::Value,
    ) -> Result<String, String> {
        match tool_name {
            "ListMcpResourcesTool" => {
                let server_name = value
                    .get("server_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "missing required field: server_name".to_string())?
                    .to_string();
                let mut manager = self
                    .mcp_manager
                    .lock()
                    .map_err(|e| format!("MCP manager lock poisoned: {e}"))?;
                let resources = self
                    .tokio_rt
                    .block_on(manager.list_resources(&server_name))
                    .map_err(|e| e.to_string())?;
                serde_json::to_string_pretty(&resources).map_err(|e| e.to_string())
            }
            "ReadMcpResourceTool" => {
                let server_name = value
                    .get("server_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "missing required field: server_name".to_string())?
                    .to_string();
                let uri = value
                    .get("uri")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "missing required field: uri".to_string())?
                    .to_string();
                let mut manager = self
                    .mcp_manager
                    .lock()
                    .map_err(|e| format!("MCP manager lock poisoned: {e}"))?;
                let result = self
                    .tokio_rt
                    .block_on(manager.read_resource(&server_name, &uri))
                    .map_err(|e| e.to_string())?;
                serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
            }
            other => Err(format!("unknown MCP resource tool: {other}")),
        }
    }

    fn execute_lsp_tool(&mut self, value: &serde_json::Value) -> Result<String, String> {
        let action = value
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "LSPTool: missing required field `action`".to_string())?;
        let file_path_str = value
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "LSPTool: missing required field `file_path`".to_string())?;
        let file_path = std::path::PathBuf::from(file_path_str);

        let manager_guard = self
            .lsp_manager
            .lock()
            .map_err(|e| format!("LSP manager lock poisoned: {e}"))?;
        let manager = manager_guard
            .as_ref()
            .ok_or_else(|| "LSPTool: no LSP servers configured — add `lspServers` to your .anvil/settings.json".to_string())?;

        match action {
            "diagnostics" => {
                let diagnostics = self
                    .tokio_rt
                    .block_on(manager.collect_workspace_diagnostics())
                    .map_err(|e| e.to_string())?;
                if diagnostics.is_empty() {
                    return Ok("No diagnostics found across the workspace.".to_string());
                }
                let mut lines = vec![format!(
                    "{} diagnostic(s) across {} file(s):",
                    diagnostics.total_diagnostics(),
                    diagnostics.files.len()
                )];
                for file in &diagnostics.files {
                    for diag in &file.diagnostics {
                        let severity = match diag.severity {
                            Some(lsp_types::DiagnosticSeverity::ERROR) => "error",
                            Some(lsp_types::DiagnosticSeverity::WARNING) => "warning",
                            Some(lsp_types::DiagnosticSeverity::INFORMATION) => "info",
                            Some(lsp_types::DiagnosticSeverity::HINT) => "hint",
                            _ => "unknown",
                        };
                        lines.push(format!(
                            "  {}:{}:{} [{}] {}",
                            file.path.display(),
                            diag.range.start.line + 1,
                            diag.range.start.character + 1,
                            severity,
                            diag.message.replace('\n', " ")
                        ));
                    }
                }
                Ok(lines.join("\n"))
            }
            "definition" => {
                let line = value
                    .get("line")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| "LSPTool: `line` is required for the definition action".to_string())?
                    .min(u64::from(u32::MAX)) as u32;
                let character = value
                    .get("character")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| "LSPTool: `character` is required for the definition action".to_string())?
                    .min(u64::from(u32::MAX)) as u32;
                let position = lsp_types::Position::new(line, character);
                let locations = self
                    .tokio_rt
                    .block_on(manager.go_to_definition(&file_path, position))
                    .map_err(|e| e.to_string())?;
                if locations.is_empty() {
                    return Ok("No definition found.".to_string());
                }
                let lines = locations
                    .iter()
                    .map(|loc| format!("  {loc}"))
                    .collect::<Vec<_>>();
                Ok(format!("Definition(s):\n{}", lines.join("\n")))
            }
            "references" => {
                let line = value
                    .get("line")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| "LSPTool: `line` is required for the references action".to_string())?
                    .min(u64::from(u32::MAX)) as u32;
                let character = value
                    .get("character")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| "LSPTool: `character` is required for the references action".to_string())?
                    .min(u64::from(u32::MAX)) as u32;
                let position = lsp_types::Position::new(line, character);
                let locations = self
                    .tokio_rt
                    .block_on(manager.find_references(&file_path, position, true))
                    .map_err(|e| e.to_string())?;
                if locations.is_empty() {
                    return Ok("No references found.".to_string());
                }
                let lines = locations
                    .iter()
                    .map(|loc| format!("  {loc}"))
                    .collect::<Vec<_>>();
                Ok(format!("{} reference(s):\n{}", locations.len(), lines.join("\n")))
            }
            "open" => {
                let content = value
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "LSPTool: `content` is required for the open action".to_string())?;
                self.tokio_rt
                    .block_on(manager.open_document(&file_path, content))
                    .map_err(|e| e.to_string())?;
                Ok(format!("Opened {} in LSP server.", file_path.display()))
            }
            "close" => {
                self.tokio_rt
                    .block_on(manager.close_document(&file_path))
                    .map_err(|e| e.to_string())?;
                Ok(format!("Closed {} in LSP server.", file_path.display()))
            }
            other => Err(format!("LSPTool: unknown action `{other}`. Valid actions: diagnostics, definition, references, open, close")),
        }
    }

    // ── Agent / Team helpers ───────────────────────────────────────────────

    /// Parse the JSON manifest returned by `execute_agent` and register a
    /// mirror `AgentHandle` in the `AgentManager` so the TUI panel stays in
    /// sync with the background thread already spawned by `spawn_agent_job`.
    ///
    /// The actual LLM work happens in the thread spawned by the tools crate;
    /// this method only creates a TUI-visible tracking entry.
    fn register_agent_in_manager(
        &mut self,
        manifest_json: &str,
        input: &serde_json::Value,
    ) {
        // Parse enough fields from the manifest to populate the handle.
        let parsed: serde_json::Value =
            match serde_json::from_str(manifest_json) {
                Ok(v) => v,
                Err(_) => return,
            };

        let agent_name = parsed
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("agent")
            .to_string();

        let subagent_type_str = parsed
            .get("subagentType")
            .and_then(|v| v.as_str())
            .or_else(|| input.get("subagent_type").and_then(|v| v.as_str()))
            .unwrap_or("General");

        let description = parsed
            .get("description")
            .and_then(|v| v.as_str())
            .or_else(|| input.get("description").and_then(|v| v.as_str()))
            .unwrap_or("sub-agent task")
            .to_string();

        let agent_type = agents::AgentType::parse(subagent_type_str);

        // The tools crate already spawned the worker thread; here we just
        // register a handle that polls the output file / manifest to detect
        // completion.  We use a lightweight sentinel runner that waits until
        // the manifest's status changes from "running".
        let output_file = parsed
            .get("outputFile")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let manifest_file = parsed
            .get("manifestFile")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let tui_tx: Option<TuiSender> = self
            .tui_slot
            .lock()
            .ok()
            .and_then(|guard| guard.clone());

        let mut mgr = self
            .agent_manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        mgr.spawn(
            agent_name.clone(),
            agent_type,
            description,
            move |sender| {
                use std::time::{Duration, Instant};

                let start = Instant::now();
                sender.send_line(format!("Agent `{agent_name}` started."));

                // Poll the manifest file until the status changes away from
                // "running", or until a 5-minute timeout.
                let timeout = Duration::from_secs(300);
                loop {
                    std::thread::sleep(Duration::from_millis(500));
                    if start.elapsed() > timeout {
                        sender.send_line("Agent timed out waiting for completion.");
                        return agents::AgentResult {
                            output: "timed out".to_string(),
                            success: false,
                            duration: start.elapsed(),
                        };
                    }
                    let status = std::fs::read_to_string(&manifest_file)
                        .ok()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                        .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(String::from))
                        .unwrap_or_else(|| "running".to_string());

                    if status == "completed" || status == "failed" {
                        let output = std::fs::read_to_string(&output_file)
                            .unwrap_or_default();
                        let success = status == "completed";
                        // Forward a few lines of the output to the TUI.
                        for line in output.lines().take(10) {
                            sender.send_line(line);
                        }
                        // Also push a system message to the TUI if active.
                        if let Some(ref tx) = tui_tx {
                            let preview: String = output
                                .lines()
                                .take(3)
                                .collect::<Vec<_>>()
                                .join(" | ");
                            tx.send(TuiEvent::System(format!(
                                "Agent `{agent_name}` {status}: {preview}"
                            )));
                        }
                        return agents::AgentResult {
                            output,
                            success,
                            duration: start.elapsed(),
                        };
                    }
                }
            },
        );
    }

    /// Spawn a real LLM worker thread for a `TeamDelegate` call, in addition
    /// to the shell-task stub already created by `run_team_delegate`.
    ///
    /// Reads `team_id`, `member_name`, and `prompt` from the tool input JSON.
    /// Spawns a thread that builds its own `ProviderClient` for the member's
    /// configured model (or the session default) and runs a single-turn
    /// conversation with the delegated prompt.
    fn spawn_team_delegate_agent(&mut self, input: &serde_json::Value) {
        let member_name = input
            .get("member_name")
            .and_then(|v| v.as_str())
            .unwrap_or("member")
            .to_string();
        let team_id = input
            .get("team_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if prompt.is_empty() {
            return;
        }

        // Look up the member's configured model from the TeamManager.
        let member_model: Option<String> = {
            use runtime::TeamManager;
            TeamManager::global()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_team(&team_id)
                .and_then(|t| t.members.iter().find(|m| m.name == member_name))
                .and_then(|m| m.model.clone())
        };

        let model = member_model.unwrap_or_else(|| self.model.clone());
        let task_desc = format!("TeamDelegate → {member_name} in team {team_id}");

        let tui_tx: Option<TuiSender> = self
            .tui_slot
            .lock()
            .ok()
            .and_then(|guard| guard.clone());

        let mut mgr = self
            .agent_manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        mgr.spawn(
            member_name.clone(),
            agents::AgentType::Custom(format!("team/{team_id}")),
            task_desc,
            move |sender| {
                use std::time::Instant;

                let start = Instant::now();
                sender.send_line(format!(
                    "TeamDelegate: {member_name} starting with model {model}"
                ));

                // Build a fresh provider client for this member's model.
                let client = match build_provider_client(&model) {
                    Ok(c) => c,
                    Err(e) => {
                        let msg = format!("failed to build provider client: {e}");
                        sender.send_line(&msg);
                        return agents::AgentResult {
                            output: msg,
                            success: false,
                            duration: start.elapsed(),
                        };
                    }
                };

                // Run a single blocking request.
                let rt = match tokio::runtime::Runtime::new() {
                    Ok(r) => r,
                    Err(e) => {
                        let msg = format!("failed to create tokio runtime: {e}");
                        sender.send_line(&msg);
                        return agents::AgentResult {
                            output: msg,
                            success: false,
                            duration: start.elapsed(),
                        };
                    }
                };

                let messages = vec![InputMessage {
                    role: "user".to_string(),
                    content: vec![InputContentBlock::Text { text: prompt.clone() }],
                }];
                let request = MessageRequest {
                    model: model.clone(),
                    max_tokens: 4096,
                    messages,
                    system: None,
                    tools: None,
                    tool_choice: None,
                    stream: false,
                };

                let response_text = match rt.block_on(client.send_message(&request)) {
                    Ok(resp) => {
                        // Extract text from the response content blocks.
                        resp.content
                            .iter()
                            .filter_map(|block| match block {
                                OutputContentBlock::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("")
                    }
                    Err(e) => {
                        let msg = format!("TeamDelegate LLM error: {e}");
                        sender.send_line(&msg);
                        return agents::AgentResult {
                            output: msg,
                            success: false,
                            duration: start.elapsed(),
                        };
                    }
                };

                for line in response_text.lines().take(20) {
                    sender.send_line(line);
                }
                if let Some(ref tx) = tui_tx {
                    let preview: String = response_text
                        .lines()
                        .take(3)
                        .collect::<Vec<_>>()
                        .join(" | ");
                    tx.send(TuiEvent::System(format!(
                        "Team member `{member_name}` completed: {preview}"
                    )));
                }

                agents::AgentResult {
                    output: response_text,
                    success: true,
                    duration: start.elapsed(),
                }
            },
        );
    }
}

pub(crate) fn permission_policy(mode: PermissionMode, tool_registry: &GlobalToolRegistry) -> PermissionPolicy {
    tool_registry.permission_specs(None).into_iter().fold(
        PermissionPolicy::new(mode),
        |policy, (name, required_permission)| {
            policy.with_tool_requirement(name, required_permission)
        },
    )
}

pub(crate) fn convert_messages(messages: &[ConversationMessage]) -> Vec<InputMessage> {
    messages
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::System | MessageRole::User | MessageRole::Tool => "user",
                MessageRole::Assistant => "assistant",
            };
            let content = message
                .blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => InputContentBlock::Text { text: text.clone() },
                    ContentBlock::Image { media_type, data } => InputContentBlock::Image {
                        source: ImageSource {
                            kind: ImageSourceKind::Base64,
                            media_type: media_type.clone(),
                            data: data.clone(),
                        },
                    },
                    ContentBlock::ToolUse { id, name, input } => InputContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::from_str(input)
                            .unwrap_or_else(|_| serde_json::json!({ "raw": input })),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id,
                        output,
                        is_error,
                        ..
                    } => InputContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: vec![ToolResultContentBlock::Text {
                            text: output.clone(),
                        }],
                        is_error: *is_error,
                    },
                })
                .collect::<Vec<_>>();
            (!content.is_empty()).then(|| InputMessage {
                role: role.to_string(),
                content,
            })
        })
        .collect()
}
