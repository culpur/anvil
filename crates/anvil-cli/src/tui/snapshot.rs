//! `LayoutSnapshot` — a frozen, heap-owned copy of all state that `draw()` reads.
//!
//! Collected by `AnvilTui::collect_snapshot()` before `terminal.draw()` takes
//! ownership of the terminal borrow. The snapshot lets the draw closure stay a
//! pure function of its inputs (no `self` access inside the closure), which is
//! the prerequisite for extracting per-layout renderers.
//!
//! v2.2.16: This struct is the boundary used by the layout system. Once the
//! renderer is split into `layouts/{vertical_split,three_pane,journal}.rs`,
//! each renderer will accept `&LayoutSnapshot` instead of `&AnvilTui`.

use std::time::Duration;

use ratatui::text::Line;
use runtime::Theme;
use runtime::theme::StatusLineConfig;

use super::configure_types::{ConfigureState, ConfigureData};
use super::list_viewport::ListViewport;
use super::modals::confirm::ConfirmRenderSnapshot;
use super::modals::password::PasswordRenderSnapshot;
use super::provider_login::ProviderLoginRenderSnapshot;
use super::redraw::DirtyRegions;
use super::scrollback::ScrollbackState;
use super::ssh_form::SshFormState;
use super::state::LogEntry;

/// A frozen copy of all the state `draw()` needs, collected before
/// `terminal.draw(|frame| { ... })` takes the mutable terminal borrow.
///
/// All fields use owned types so the snapshot is `'static`-safe and can be
/// passed freely between functions and threads.
pub(crate) struct LayoutSnapshot {
    // ── Active-tab snapshot (per-tab data) ────────────────────────────────
    /// Frozen copy of the active tab's conversation log.
    pub log_snapshot: Vec<LogEntry>,
    /// Whether tool calls render in verbose (expanded) mode.
    pub transcript_verbose: bool,
    /// Streaming partial text from the assistant (not yet a committed `LogEntry`).
    pub pending: String,
    /// Current thinking-phase label (e.g. "Thinking…").
    pub think: String,
    /// Which spinner glyph to show for the thinking indicator.
    pub think_frame: &'static str,
    /// Elapsed seconds since the active tab's spinner started.
    /// `0.0` when no spinner is active (i.e. `think` is empty).
    /// Used by the spinner color-warm logic (#558, CC-141-F).
    pub think_elapsed_secs: f64,
    /// Seconds before the spinner shifts from green to amber (default 10).
    /// Sourced from `ANVIL_SPINNER_WARN_SECS` at startup.
    pub spinner_warn_secs: u64,
    /// Seconds before the spinner shifts from amber to red (default 30).
    /// Sourced from `ANVIL_SPINNER_ERROR_SECS` at startup.
    pub spinner_error_secs: u64,
    /// Content of the text input box.
    pub input_text: String,
    /// Total number of queued messages (pending_submit + message_queue length).
    pub queued_count: usize,
    /// Up to 3 preview strings for the queued-message indicator.
    pub queued_preview: Vec<String>,
    /// Byte offset of the cursor in `input_text`.
    pub cursor_pos: usize,
    /// Deprecated scroll field (legacy; will be removed when layout split lands).
    pub scroll: usize,
    /// Current scrollback navigation state for the active tab.
    pub scrollback_state: ScrollbackState,
    /// True when the scrollback is in live (bottom-tracking) mode.
    pub scrollback_is_live: bool,
    /// Model name for the active tab (e.g. "claude-sonnet-4-6").
    pub model: String,
    /// Session identifier for the active tab.
    pub session_id: String,
    /// Cumulative input token count for the active tab.
    pub input_tokens: u32,
    /// Cumulative output token count for the active tab.
    pub output_tokens: u32,
    /// Time elapsed since the active tab's session started.
    pub elapsed: Duration,
    /// Whether the slash-command completion popup is visible.
    pub completion_visible: bool,
    /// Currently highlighted row in the completion popup.
    pub completion_selected: usize,
    /// Match entries: `(insert_text, hint, is_header, is_free_text)`.
    pub completion_matches: Vec<(String, String, bool, bool)>,
    /// First visible row in the completion popup (viewport offset).
    pub completion_view_offset: usize,

    // ── Tab-bar snapshot ──────────────────────────────────────────────────
    /// One entry per open tab: `(id, name, is_active, has_unread, has_pending_permission)`.
    pub tab_infos: Vec<(usize, String, bool, bool, bool)>,
    /// Pending permission modal data for the active tab, if any.
    /// Fields: `(tool_name, required_mode, current_mode, input_summary)`.
    pub active_permission_modal: Option<(String, String, String, String)>,

    // ── Global TUI snapshot (AnvilTui-level fields) ───────────────────────
    /// Current git branch name (empty when not in a git repo).
    pub git_branch: String,
    /// Compact diff stats string (e.g. "+12,-5"; empty when clean).
    pub git_diff_stats: String,
    /// Permission mode display label (e.g. "default", "strict").
    pub permission_mode: String,
    /// Maximum context window size for the active model.
    pub context_max_tokens: u32,
    /// QMD index status line (empty when QMD is not running).
    pub qmd_status: String,
    /// Last archive/compaction status message shown to the user.
    pub last_archive_status: String,
    /// Update notification message (empty = no update available).
    pub update_available: String,
    /// Current configure overlay state (Inactive = overlay not shown).
    pub configure_state: ConfigureState,
    /// Live values shown in the configure overlay.
    pub configure_data: ConfigureData,
    /// Scroll viewport position for the active configure screen.
    pub configure_viewport: ListViewport,
    /// Active colour theme.
    pub theme: Theme,
    /// Whether the agent panel is visible.
    pub agent_panel_visible: bool,
    /// Agent panel rows: `(id, type_label, task, elapsed_str, icon)`.
    pub agent_rows: Vec<(usize, String, String, String, &'static str)>,
    /// Remote-control session URL (empty = session inactive).
    pub remote_url: String,
    /// Remote-control pairing code (shown while awaiting a client).
    pub remote_code: String,
    /// Status line display configuration.
    pub sl_config: StatusLineConfig,
    /// Whether extended thinking mode is enabled.
    pub thinking_enabled: bool,
    /// Lines added in the current session (from git diff).
    pub lines_added: u32,
    /// Lines removed in the current session (from git diff).
    pub lines_removed: u32,
    /// Active effort level label for the status line (e.g. "medium").
    pub effort_level: String,

    // ── Pre-rendered SSH screen (None when the active tab is not SSH) ─────
    /// Rendered vt100 grid lines + footer lines for an SSH tab, or `None`.
    pub ssh_screen: Option<(Vec<Line<'static>>, Vec<Line<'static>>)>,
    /// True when the active tab is an SSH tab (`ssh_screen.is_some()`).
    pub is_ssh_tab: bool,

    // ── SSH connection form ────────────────────────────────────────────────
    /// Cloned SSH form state when the modal is open; `None` otherwise.
    pub ssh_form_snapshot: Option<SshFormState>,

    // ── Provider-login modal (#578) ────────────────────────────────────────
    /// Snapshot of the provider-login modal when it is open.
    /// Stored as a `Box` to avoid cloning large `mpsc::Receiver` (which is
    /// not `Clone`); we use `Option<Box<ProviderLoginModal>>` and the draw
    /// closure borrows it.  The actual `Receiver` inside `OAuthWaiting` stays
    /// in `AnvilTui.provider_login_modal` — the snapshot carries a shallow
    /// copy of every renderable field via `ProviderLoginModal::render_snapshot`.
    pub provider_login_modal_snapshot: Option<ProviderLoginRenderSnapshot>,

    // ── Confirm / password modals (task #627) ─────────────────────────────
    /// Cloned ConfirmModal state when the overlay is open; `None` otherwise.
    pub confirm_modal_snapshot: Option<ConfirmRenderSnapshot>,
    /// Cloned PasswordModal render snapshot (mask length + error, never the
    /// raw buffer) when the overlay is open; `None` otherwise.
    pub password_modal_snapshot: Option<PasswordRenderSnapshot>,

    // ── Pre-fetched scrollback view (for historical view mode) ────────────
    /// Lines to display when the user has scrolled back in history.
    /// `None` when in live view (scrollback tracks the bottom).
    pub scrollback_view_lines: Option<Vec<String>>,

    // ── Click-geometry helpers (initialised to empty; filled during draw) ─
    /// Whether the session has more than one tab open (enables close glyph).
    pub can_close_tab: bool,

    // ── Cross-tab aggregates (computed once per frame in collect_snapshot) ─
    /// Number of tabs that currently have an in-flight turn (streaming).
    pub running_tab_count: usize,
    /// Total pending permission requests across ALL tabs.
    pub pending_permission_count: usize,
    /// Sum of per-tab session cost in USD across ALL tabs.
    /// Each tab's cost is estimated from its input/output token counts
    /// and the active model's pricing table.
    pub total_session_cost_usd: f64,

    // ── Seven-layer memory surface (task #594 / BUG-13) ─────────────────────
    // These power the rail's MEMORY section. All fields are best-effort
    // counts pulled from on-disk caches in `~/.anvil/`; they degrade to zero
    // when the relevant store is absent.
    /// Working memory: count of user+assistant turns in the active tab's log.
    pub memory_working_turns: usize,
    /// Working memory: cumulative input+output tokens for the active tab.
    pub memory_working_tokens: u32,
    /// Episodic memory: number of session files persisted in
    /// `~/.anvil/sessions/`. Includes the live session.
    pub memory_episodic_sessions: usize,
    /// Semantic memory: number of QMD collections (currently 1 when the
    /// `anvil-semantic` collection is initialised, 0 otherwise).
    pub memory_semantic_collections: usize,
    /// Semantic memory: number of archived semantic documents indexed via
    /// `~/.anvil/semantic/` (best-effort `read_dir` count).
    pub memory_semantic_archives: usize,
    /// Procedural memory: number of skill `.md` files under `~/.anvil/skills/`.
    pub memory_procedural_skills: usize,
    /// Procedural memory: number of plugin subdirectories under
    /// `~/.anvil/plugins/`.
    pub memory_procedural_plugins: usize,
    /// Reflective memory: number of daily summary JSON files under
    /// `~/.anvil/daily/`.
    pub memory_reflective_daily: usize,
    /// Permission memory: number of stored allow decisions for the current
    /// project (sourced from `PermissionMemory::load`).
    pub memory_permission_decisions: usize,

    // ── QMD surface ────────────────────────────────────────────────────────
    /// QMD: latest indexed session id (best-effort; falls back to the live
    /// session id when no archived sessions are present).
    pub qmd_latest_session_id: Option<String>,
    /// QMD: age in days of the latest indexed session (0 for the live one).
    pub qmd_latest_age_days: Option<u32>,
    /// QMD: total archived documents indexed (`QmdStatus::total_docs`).
    pub qmd_archive_count: u32,

    // ── Permission + cost labels (formatted strings for the rail) ──────────
    /// Pretty label for the active permission mode (e.g. "bypass on",
    /// "default", "plan").
    pub permission_mode_label: String,
    /// Cost-source label: "local", "cloud", "metered", or "OAuth".
    pub cost_provider_label: String,

    // ── Build / version ────────────────────────────────────────────────────
    /// `env!("CARGO_PKG_VERSION")` — same value the welcome banner shows.
    pub build_version: String,
    /// Short git SHA (first 7 chars of `env!("GIT_SHA")`).
    pub build_git_sha_short: String,

    // ── Context bar (lifted out of classic status line) ────────────────────
    /// Tokens currently consumed by the active tab (input + output).
    pub context_used_tokens: u32,
    /// Effective context window size for the active model.
    pub context_limit_tokens: u32,
    /// Session percent of context used (0.0–100.0).
    pub session_pct: f32,
    /// Whole minutes elapsed in the current session block.
    pub block_minutes: u32,

    // ── Task #622: redraw gating for photosensitivity / Gnome Terminal flash ──
    /// The dirty set captured at the moment the frame was committed to the
    /// scheduler (see `RedrawScheduler::last_committed_dirty`). Layout
    /// renderers consult this to decide whether the top-level full-screen
    /// `Clear` widget is justified for THIS frame. Defaults to
    /// `DirtyRegions::ALL` so legacy/direct draw paths keep the conservative
    /// wipe behavior.
    pub dirty_regions: DirtyRegions,
}

#[cfg(test)]
impl LayoutSnapshot {
    /// Test-only constructor with reasonable defaults across all fields.
    ///
    /// Use this in renderer goldens to call the real `render_rail` /
    /// `render_deck` with a minimal-but-complete snapshot. Test code can
    /// override individual fields after construction.
    pub(crate) fn test_default() -> Self {
        Self {
            log_snapshot: Vec::new(),
            transcript_verbose: false,
            pending: String::new(),
            think: String::new(),
            think_frame: "",
            think_elapsed_secs: 0.0,
            spinner_warn_secs: 10,
            spinner_error_secs: 30,
            input_text: String::new(),
            queued_count: 0,
            queued_preview: Vec::new(),
            cursor_pos: 0,
            scroll: 0,
            scrollback_state: super::scrollback::ScrollbackState::live(),
            scrollback_is_live: true,
            model: String::new(),
            session_id: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            elapsed: Duration::ZERO,
            completion_visible: false,
            completion_selected: 0,
            completion_matches: Vec::new(),
            completion_view_offset: 0,
            tab_infos: Vec::new(),
            active_permission_modal: None,
            git_branch: String::new(),
            git_diff_stats: String::new(),
            permission_mode: String::new(),
            context_max_tokens: 0,
            qmd_status: String::new(),
            last_archive_status: String::new(),
            update_available: String::new(),
            configure_state: ConfigureState::Inactive,
            configure_data: ConfigureData::default(),
            configure_viewport: super::list_viewport::ListViewport::new(),
            theme: Theme::default_theme(),
            agent_panel_visible: false,
            agent_rows: Vec::new(),
            remote_url: String::new(),
            remote_code: String::new(),
            sl_config: StatusLineConfig::default(),
            thinking_enabled: false,
            lines_added: 0,
            lines_removed: 0,
            effort_level: String::new(),
            ssh_screen: None,
            is_ssh_tab: false,
            ssh_form_snapshot: None,
            provider_login_modal_snapshot: None,
            confirm_modal_snapshot: None,
            password_modal_snapshot: None,
            scrollback_view_lines: None,
            can_close_tab: false,
            running_tab_count: 0,
            pending_permission_count: 0,
            total_session_cost_usd: 0.0,
            memory_working_turns: 0,
            memory_working_tokens: 0,
            memory_episodic_sessions: 0,
            memory_semantic_collections: 0,
            memory_semantic_archives: 0,
            memory_procedural_skills: 0,
            memory_procedural_plugins: 0,
            memory_reflective_daily: 0,
            memory_permission_decisions: 0,
            qmd_latest_session_id: None,
            qmd_latest_age_days: None,
            qmd_archive_count: 0,
            permission_mode_label: String::new(),
            cost_provider_label: String::new(),
            build_version: String::new(),
            build_git_sha_short: String::new(),
            context_used_tokens: 0,
            context_limit_tokens: 0,
            session_pct: 0.0,
            block_minutes: 0,
            // Task #622: default test snapshots to ALL so existing snapshot
            // tests keep their conservative full-clear behavior.
            dirty_regions: DirtyRegions::ALL,
        }
    }
}
