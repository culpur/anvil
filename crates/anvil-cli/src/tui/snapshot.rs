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

    // ── Pre-fetched scrollback view (for historical view mode) ────────────
    /// Lines to display when the user has scrolled back in history.
    /// `None` when in live view (scrollback tracks the bottom).
    pub scrollback_view_lines: Option<Vec<String>>,

    // ── Click-geometry helpers (initialised to empty; filled during draw) ─
    /// Whether the session has more than one tab open (enables close glyph).
    pub can_close_tab: bool,
}
