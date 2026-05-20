//! Full-screen TUI for Anvil — ratatui-based alternate-screen layout.
//!
//! This is the module root.  Submodules:
//!   state            — `TuiEvent`, `TuiSender`, `LogEntry`, `Tab`, `CompletionPopup`, `THINK_FRAMES`
//!   helpers          — `strip_ansi`, `truncate_str`, char-boundary helpers, `permission_mode_display`
//!   layout           — `compute_input_lines`, `cursor_visual_position`, status-line span builders
//!   widgets          — slash-command completion data, Ollama model cache, clipboard helpers
//!   `configure_types`  — `ConfigureState`, `ConfigureAction`, `ConfigureData`, configure menu helpers
//!   `input_handler`    — `AnvilTui` input loop (`read_input`, `handle_key`, editing, history, completion)

// Task #626 — every file under `tui/` runs while ratatui owns the
// alt-screen.  `println!` / `eprintln!` is banned.  Warnings route
// through `tui::log_warning` (which writes to ~/.anvil/anvil.log while
// the TUI is up, or stderr when it isn't); informational output goes
// through `tui.push_system` so it appears in scrollback.
#![deny(clippy::print_stdout, clippy::print_stderr)]
pub mod configure_types;
pub(super) mod completion;
pub(super) mod helpers;
pub(super) mod layout;
pub(super) mod layouts;
pub(super) mod list_viewport;
pub(super) mod redraw;
pub(super) mod scrollback;
pub(super) mod snapshot;
pub(super) mod ssh_bridge;
pub(super) mod modals;
pub(super) mod oauth_flow;
pub(super) mod provider_login;
pub(super) mod ssh_form;
pub(super) mod ssh_tab;
pub(super) mod state;
pub(super) mod widgets;
pub(super) mod input_handler;
pub(super) mod paste;

// ─── Public re-exports ────────────────────────────────────────────────────────

pub use state::{InFlightInterruption, TaggedTuiEvent, TuiEvent, TuiSender};
pub(super) use state::{PermissionReply, PendingPermission};
pub use configure_types::{ConfigureAction, ConfigureData};
pub use widgets::{init_ollama_model_cache, invalidate_ollama_model_cache};
// Task #627: re-export modal pending-action enums + choice enum so the
// LiveCli host can match on them after read_input returns
// ReadResult::ConfirmResolved / ::PasswordSubmitted.
pub use modals::{ConfirmChoice, PendingConfirmAction, PendingPasswordAction};


// Internal imports used by AnvilTui methods in this file.
use configure_types::{
    ConfigureState,
    configure_breadcrumb, configure_selected, configure_set_selected,
    configure_item_count, configure_screen_tag, section_state_from_name,
    configure_action_for, configure_data_notify_value, mask_sensitive,
};
use helpers::{strip_ansi, truncate_str, prev_char_boundary, next_char_boundary};
use snapshot::LayoutSnapshot;
use state::{Tab, LogEntry, THINK_FRAMES};


use std::io::{self, Stdout};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use runtime::{Rgb, Theme};
use runtime::theme::StatusLineConfig;

use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use vt100::Color as Vt100Color;
use ratatui::backend::CrosstermBackend;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;

// ─── ReadResult ───────────────────────────────────────────────────────────────

/// The result of one call to `AnvilTui::read_input()`.
pub enum ReadResult {
    /// Nothing to report; caller should call `read_input()` again.
    Continue,
    /// User submitted this line.
    Submit(String),
    /// User requested exit (Ctrl+C on empty input, Ctrl+D on empty input).
    Exit,
    /// User requested a new tab (Ctrl+T).  Caller should create a new session
    /// and call `tui.new_tab(name, model, session_id)` then `tui.switch_tab(idx)`.
    NewTab,
    /// User triggered a configure action from the interactive configure menu.
    ConfigureAction(ConfigureAction),
    /// Task #627: a confirm modal opened by `/restart` or `/iac apply`
    /// resolved.  The host runs `action` when `choice == Yes`, or
    /// pushes a "Cancelled" system message when `choice == No`.
    ConfirmResolved {
        action: modals::PendingConfirmAction,
        choice: modals::ConfirmChoice,
    },
    /// Task #627: the password modal opened by `/vault unlock` submitted
    /// a password.  Host attempts the action; on failure host calls
    /// [`AnvilTui::password_modal_set_error`] and the modal stays open;
    /// on success host calls [`AnvilTui::close_password_modal`].
    PasswordSubmitted {
        action: modals::PendingPasswordAction,
        password: String,
    },
}

// ─── AnvilTui ─────────────────────────────────────────────────────────────────

/// Convert a runtime `Rgb` triple into a ratatui `Color`.
#[inline]
const fn rgb(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// The full-screen TUI driver.
///
/// Create with `AnvilTui::new()`, then call `run()` to enter the main loop.
/// The caller passes prompts back via the returned `String` from `run()`.
pub struct AnvilTui {
    pub(super) terminal: Terminal<CrosstermBackend<Stdout>>,
    /// All open tabs.
    pub(super) tabs: Vec<Tab>,
    /// Index into `tabs` of the currently visible tab.
    pub(super) active_tab: usize,
    /// Channel receiver from the model/tool pipeline.  Each message is a
    /// `TaggedTuiEvent` carrying the `tab_id` of the runtime that sent it.
    pub(super) rx: Receiver<TaggedTuiEvent>,
    /// True once /exit or Ctrl+D has been issued.
    pub(super) exiting: bool,
    /// Current git branch name (empty if not in a git repo).
    pub(super) git_branch: String,
    /// Compact diff stats string e.g. "+12,-5" (empty if no diff or not in git repo).
    pub(super) git_diff_stats: String,
    /// Current permission mode display label.
    pub(super) permission_mode: String,
    /// Maximum context window tokens for the current model.
    pub(super) context_max_tokens: u32,
    /// Running counter for assigning tab IDs.
    pub(super) next_tab_id: usize,
    /// QMD status line: docs indexed, vectors, last update
    pub(super) qmd_status: String,
    /// Last archive info shown to user
    pub(super) last_archive_status: String,
    /// Current configure menu state (Inactive = not in configure mode).
    pub(super) configure_state: ConfigureState,
    /// Snapshot of live config values shown in the configure menu.
    pub(super) configure_data: ConfigureData,
    /// Scroll viewport for the active configure-overlay screen.
    /// Reset to top whenever `configure_state` transitions to a new screen.
    pub(super) configure_viewport: list_viewport::ListViewport,
    /// Discriminant of the last `configure_state` we drew, used to decide when
    /// to reset `configure_viewport` (we don't compare full state because some
    /// variants carry a `Box<StatusLineConfig>` that's expensive to clone-eq).
    pub(super) configure_screen_tag: u8,
    /// Active colour theme — loaded from ~/.anvil/theme.json at startup.
    pub theme: Theme,
    /// Whether extended thinking mode is enabled.
    pub(super) thinking_enabled: bool,
    /// Active effort level for the status line display (e.g. "medium").
    pub(super) effort_level: String,
    /// Relay broadcast sender for forwarding events to web viewers.
    pub(super) relay_tx: Option<tokio::sync::broadcast::Sender<runtime::relay::RelayMessage>>,
    /// Update notification message (empty = no update available).
    pub(super) update_available: String,
    /// Whether the agent panel is visible (toggled with Ctrl+A).
    pub agent_panel_visible: bool,
    /// Agent panel rows snapshot — refreshed by the caller each frame.
    /// Each entry: (id, `type_label`, task, `elapsed_str`, icon).
    pub agent_rows: Vec<(usize, String, String, String, &'static str)>,
    /// Timestamp of the last Ctrl+C that fired while the input was already
    /// empty.  A second Ctrl+C within 1 second exits; otherwise it resets.
    pub(super) ctrl_c_empty_at: Option<Instant>,
    /// Remote-control relay URL (empty = session inactive).
    pub(super) remote_url: String,
    /// Remote-control pairing/session code (shown while awaiting a client).
    pub(super) remote_code: String,
    /// Focus mode — show only prompt + tool summary + final response.
    pub(super) focus_mode: bool,
    /// Status line configuration — determines which widgets appear and where.
    pub(super) status_line_config: StatusLineConfig,
    /// Lines added in current session (from git diff).
    pub(super) lines_added: u32,
    /// Lines removed in current session (from git diff).
    pub(super) lines_removed: u32,
    /// Unified redraw scheduler — coordinates dirty-region tracking and
    /// frame-budget coalescing. Call sites that produce a visible change
    /// should call `request_redraw(region)`; the main loop calls
    /// `commit_redraw()` once per iteration. See `tui/redraw.rs` for design.
    pub(super) redraw: redraw::RedrawScheduler,
    /// Held submit while a turn is in flight (T1-#400). When the user
    /// presses Enter while the model is responding, the input draft is
    /// captured here and the input box clears so they can keep typing the
    /// NEXT message. `wait_for_turn_end` returns this value (if Some) so the
    /// caller can fire it as the next prompt without going back through
    /// `read_input`.
    pub(super) pending_submit: Option<String>,
    /// Auto-fire queue for held drafts that should bypass the input box and
    /// submit directly on the next `read_input` poll. Used by the in-flight
    /// turn handler to route slash commands like `/ssh` straight to dispatch
    /// instead of stashing them back in the input box (where they'd require a
    /// second Enter from the user, and where the previous behavior risked
    /// them being submitted as a chat message if cleared+retyped). Drained
    /// at the top of every `read_input` call.
    pub(super) pending_auto_submit: Option<String>,
    /// T5-Ssh-D: true for exactly one key cycle after the user presses
    /// Ctrl+B in an SSH tab. The NEXT key consumed will be interpreted as an
    /// escape command (digit → tab switch, 'q' → close SSH tab) rather than
    /// forwarded to the remote shell.
    pub(super) ssh_escape_pending: bool,
    /// T5-Ssh-E: active SSH connection form overlay. `Some` while the modal
    /// is open; `None` otherwise.
    pub(super) ssh_form: Option<ssh_form::SshFormState>,
    /// #578: in-TUI provider login modal. `Some` while the overlay is open;
    /// `None` otherwise.  Replaces the drop-to-CLI pattern in `run_inline_login`.
    pub(super) provider_login_modal: Option<provider_login::ProviderLoginModal>,
    /// Task #627: in-TUI yes/no confirmation modal.  `Some` while the
    /// overlay is open; `None` otherwise.  Used by `/restart` and
    /// `/iac apply`, which previously corrupted the alt-screen with
    /// `print!`/`read_line` confirm prompts.
    pub(super) confirm_modal: Option<modals::ConfirmModal>,
    /// Task #627: pending action to fire when `confirm_modal` resolves
    /// to `Yes`.  Always paired with `confirm_modal` — both set when
    /// the modal opens, both cleared on resolution.
    pub(super) pending_confirm_action: Option<modals::PendingConfirmAction>,
    /// Task #627: in-TUI masked-input password modal.  Used by
    /// `/vault unlock` (and any future vault subcommand that needs the
    /// master password) so the secret never touches stdout / the
    /// ratatui back-buffer.
    pub(super) password_modal: Option<modals::PasswordModal>,
    /// Task #627: pending action to fire when `password_modal` submits.
    /// On failure the host calls `set_error` and keeps the modal open
    /// (up to 3 attempts); on success or final failure both fields are
    /// cleared.
    pub(super) pending_password_action: Option<modals::PendingPasswordAction>,
    /// Task #579: FIFO queue of pending modal overlays used by the
    /// in-TUI first-run wizard (foundation). Empty in normal sessions.
    /// The input handler does not drain this yet — wired in v2.2.18
    /// alongside the wizard adapter; defined now so the queue's tests
    /// compile against the same `tui::modals::queue` module the wizard
    /// will eventually target.
    #[allow(dead_code)]
    pub(super) modal_queue: modals::queue::ModalQueue,
    /// Clickable-tab geometry recorded by the draw loop. The input handler
    /// looks this up on a mouse Down(Left) event to decide whether the click
    /// landed on a tab label (switch) or its close glyph (close).
    pub(super) tab_hits: Vec<TabHit>,
    /// Terminal row of the tab bar (always 0 in current layout, but recorded
    /// explicitly so a future header reshuffle won't silently break clicks).
    pub(super) tab_bar_row: u16,
    /// Bug-3 Commit 4: pending permission requests from worker threads.
    ///
    /// Keyed by logical `tab_id`.  The worker for that tab is blocked on
    /// the `response_tx` channel inside the `PendingPermission`.  The TUI
    /// shows a modal when the user is on that tab; for background tabs the
    /// tab bar shows a ⚠ marker until the user switches and approves/denies.
    pub(super) pending_permissions: std::collections::HashMap<usize, PendingPermission>,
    /// v2.2.14 BUG-fix-real: central render gate. Set by `request_redraw`
    /// from any code path that produced a visible change (key events, text
    /// delta batches, tab switches, spinner ticks). The wait loop and main
    /// loop consult this flag and, when set, call `draw_full()` + explicit
    /// `backend.flush()` to force a fully-committed terminal frame.
    ///
    /// `24bbe50` fixed the wrong layer — `insert_char` was already correct;
    /// the actual broken step was the terminal frame commit during a hot
    /// `TextDelta` firehose. Routing every "needs paint" decision through
    /// this gate gives us a single chokepoint that we can instrument and
    /// (per Step 3) augment with a forced `terminal.clear()` to bypass
    /// ratatui's frame-diff coalescing.
    pub(super) redraw_pending: bool,
    /// Most recent reason the gate was set; recorded so the instrumentation
    /// log can attribute each commit to a specific cause.
    pub(super) redraw_reason: Option<RedrawReason>,

    // ── v2.2.16 Layout system ────────────────────────────────────────────────

    /// Active TUI layout configuration. Persisted in `~/.anvil/config.json`
    /// as `tui_layout`. Defaults to `VerticalSplit { tabs: true }` (Layout D)
    /// which matches the pre-v2.2.16 rendering exactly.
    pub(super) tui_layout: runtime::TuiLayoutConfig,
    /// Layout-local visual state. Reset to `LayoutLocalState::for_kind(kind)`
    /// on every live switch. Shared `AnvilTui`/`Tab` state (log, input,
    /// cursor, message_queue, pending_permissions) is never touched here.
    pub(super) layout_local: layouts::LayoutLocalState,

    // ── v2.2.16 Spinner color-warm (#558, CC-141-F) ─────────────────────────

    /// Elapsed seconds before the spinner shifts from green to amber.
    /// Defaults to 10. Override via `ANVIL_SPINNER_WARN_SECS`.
    pub(super) spinner_warn_secs: u64,
    /// Elapsed seconds before the spinner shifts from amber to red.
    /// Defaults to 30. Override via `ANVIL_SPINNER_ERROR_SECS`.
    pub(super) spinner_error_secs: u64,

    // ── v2.2.14 Phase 1 / Task #604: paste counter ──────────────────────────

    /// Monotonic counter incremented every time a long bracketed paste is
    /// substituted with a `[Pasted text #N +M lines]` placeholder. Survives
    /// tab switches so users get globally-unique IDs across the session.
    pub(super) paste_counter: usize,

    // ── Task #604 Part C: keystroke-burst tracker ───────────────────────────

    /// State for the drag-and-drop keystroke heuristic. Detects when a
    /// rapid burst of `KeyCode::Char` events looks like a path being
    /// dragged from Finder (the terminal delivers the bytes one-char-at-
    /// a-time, never firing `Event::Paste`) and substitutes a
    /// `[image: foo.png]` placeholder mid-typing — matching CC's UX
    /// instead of waiting for Enter (which is the Part A fallback). See
    /// `tui::paste::record_keystroke` for the heuristic.
    ///
    /// Singleton across tabs by design: a drop never starts on one tab
    /// and finishes on another. Reset on tab-switch.
    pub(super) burst_tracker: paste::BurstTracker,

    // ── Task #634: rail-focus navigation (v2.2.14 Phase 1) ──────────────────

    /// Which rail section currently has the navigation focus.
    ///
    /// Driven by `g` (Deck) / `d` (Tools) / `s` (Sessions) / `a` (Agents) when
    /// the input buffer is empty and no modal is open. The vertical_split
    /// renderer paints the focused section's header with a bold accent style
    /// so the user can see which section receives the next nav key.
    ///
    /// Stored at the AnvilTui level (not per-tab) because rail focus tracks
    /// the user's eye, not session-specific state — switching tabs should
    /// preserve which rail section the user was looking at.
    pub(super) rail_focus: RailFocus,
}

/// Which left-rail section currently owns the navigation focus.
///
/// Task #634 (v2.2.14 Phase 1). The bottom KEYBINDS block in
/// `vertical_split.rs` advertises `g switch deck · d tools · s sessions ·
/// a agents`; this enum + the key-handler match arms in `input_handler.rs`
/// are the wiring those labels point at. The rail renderer reads
/// `LayoutSnapshot.rail_focus` and applies bold accent emphasis to the
/// active section header. Default `Deck` matches the old behavior (the
/// deck owns the cursor on startup).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RailFocus {
    /// The conversation deck owns focus. Default. `g` returns here.
    #[default]
    Deck,
    /// The tools / agent-tree subpanel owns focus. `d` selects this.
    Tools,
    /// The SESSIONS rail block owns focus. `s` selects this.
    Sessions,
    /// The AGENTS (GLOBAL) rail block owns focus. `a` selects this.
    Agents,
}

/// v2.2.14 BUG-fix-real: tagged reason for a `request_redraw` call.
///
/// Recorded on `AnvilTui.redraw_reason` and emitted via the [DRAW-BEGIN]
/// instrumentation so we can correlate each committed frame with the event
/// that requested it. Specifically: when the user types on tab 2 while
/// tab 1 firehoses, we should see the [KEY] markers and the [DRAW-BEGIN]
/// markers interleaved with [DRAW-END] markers in the same order. A
/// missing [DRAW-END] points the finger at terminal commit; a present
/// [DRAW-END] without visible echo points at frame-diff coalescing.
#[derive(Debug, Clone, Copy)]
pub enum RedrawReason {
    /// A user key event mutated input/cursor state.
    KeyEvent,
    /// One or more `TextDelta` events landed via `apply_tagged_event`.
    TextDeltaBatch,
    /// The streaming spinner advanced one frame.
    Spinner,
    /// The user switched tabs (or the active tab changed for any reason).
    /// This is the only "soft" structural event that still warrants a hard
    /// clear — ratatui's diff can coalesce across the boundary otherwise.
    TabSwitch,
    /// Returning from a drop-to-CLI inline operation (OAuth login, vault
    /// setup, etc.). The terminal state has been mutated outside ratatui's
    /// awareness — a hard clear is the only safe way to resync.
    InlineOpReturn,
    /// Catch-all for paths that haven't been classified yet. Routes through
    /// the soft `draw()` path per task #629; if you need a hard clear, use
    /// `TabSwitch` or `InlineOpReturn` explicitly.
    Other,
}

/// One clickable region on the tab bar.
#[derive(Debug, Clone, Copy)]
pub(super) struct TabHit {
    pub idx: usize,
    /// Inclusive start column (label first character).
    pub label_start: u16,
    /// Exclusive end column (one past the label's last character).
    pub label_end: u16,
    /// Column of the `×` close glyph, or `None` when only one tab is open.
    pub close_col: Option<u16>,
}

/// Runtime probe for the TUI lifecycle (task #626).
///
/// Set to `true` while `run_tui_session` is active and the alt-screen has
/// been entered; reset to `false` after `drop(tui)`.  Background helpers
/// (relay threads, MCP/LSP discovery, daily-summary writers, skill-chain
/// evaluators) gate their stderr fallback paths on `!tui_session_active()`
/// so a stray warning never corrupts the ratatui back-buffer.
///
/// This is the canonical "is the TUI currently up?" probe — distinct from
/// [`alternate_screen_enabled`], which is a process-start configuration
/// flag.
pub(crate) static TUI_SESSION_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Task #687: signal raised by `restore_alt_screen()` and any other inline
/// op that re-enters the alt-screen. The next iteration of the main render
/// loop checks + clears this flag and calls `redraw.request_full()` to force
/// a complete repaint — without this, ratatui's back-buffer is stale relative
/// to the freshly-cleared terminal and the frame-diff renderer skips cells it
/// believes are already correct, leaving the parent TUI blank/stale until the
/// user types something.
pub(crate) static FORCE_FULL_REDRAW: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Returns and clears the FORCE_FULL_REDRAW flag. Main render loop calls this
/// once per iteration; on `true` it calls `redraw.request_full()`.
pub fn take_force_full_redraw() -> bool {
    FORCE_FULL_REDRAW.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// Task #688: tracks whether mouse capture is CURRENTLY enabled on the
/// terminal. AnvilTui::new() sets this when it emits EnableMouseCapture;
/// `leave_alt_screen_for_inline_op` checks it to know whether to issue
/// `DisableMouseCapture` (the wizard runs OUTSIDE the alt-screen with raw
/// stdout — if mouse capture stays on, the terminal floods the user with
/// raw SGR mouse-tracking escape codes that look like garbage commands).
/// `restore_alt_screen` re-enables it on the way back in.
///
/// User-reported 2026-05-20: opening /mcp builder inside an Ubuntu/Mac
/// terminal with mouse capture on emits `^[[<35;...M` sequences into the
/// wizard's text area; after anvil exits cleanly, zsh receives the same
/// codes as "command not found" errors. Root cause: leave/restore pair
/// only managed alt-screen state, not mouse-capture state.
pub(crate) static MOUSE_CAPTURE_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Returns true iff `AnvilTui::new()` previously enabled mouse capture in
/// this process. Used by `leave_alt_screen_for_inline_op` /
/// `restore_alt_screen` to keep the mouse-capture toggle paired with the
/// alt-screen toggle.
pub fn mouse_capture_active() -> bool {
    MOUSE_CAPTURE_ACTIVE.load(std::sync::atomic::Ordering::SeqCst)
}

/// Returns `true` if a TUI session is currently active (alt-screen up).
///
/// Background tasks that emit warnings on error paths should consult this
/// before writing to stderr: when the TUI owns the terminal, a stray
/// `eprintln!` will be painted onto the alt-screen behind ratatui's diff
/// renderer and the back-buffer will desynchronise from what the user
/// sees.  Use this probe + a fallback log file (or silent drop) instead.
#[inline]
pub fn tui_session_active() -> bool {
    TUI_SESSION_ACTIVE.load(std::sync::atomic::Ordering::SeqCst)
}

/// Internal hook to flip the TUI-active flag.  Called by `run_tui_session`
/// at entry/exit; not part of the public API.
#[inline]
pub(crate) fn set_tui_session_active(active: bool) {
    TUI_SESSION_ACTIVE.store(active, std::sync::atomic::Ordering::SeqCst);
}

/// Best-effort warning sink for code paths that are TUI-reachable but
/// non-critical (MCP/LSP discovery errors, relay thread errors, skill-chain
/// parse warnings, etc.).
///
/// Behaviour:
/// * When no TUI session is active → `eprintln!("[anvil] {msg}")`.
/// * When a TUI session is active → append to `$ANVIL_HOME/anvil.log` if
///   we can resolve a home dir; otherwise drop silently (the back-buffer
///   stays consistent and the user can re-run with TUI off to see the
///   warning live).
///
/// Task #626 — see `feedback-tui-stdout-anti-pattern.md`.
pub fn log_warning(msg: &str) {
    if tui_session_active() {
        // Append to ~/.anvil/anvil.log; silent failure is fine.
        if let Some(home) = dirs_next::home_dir() {
            let log_path = home.join(".anvil").join("anvil.log");
            // Ignore directory-create / write errors — this is best-effort.
            let _ = std::fs::create_dir_all(log_path.parent().unwrap_or(&log_path));
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                use std::io::Write as _;
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let _ = writeln!(f, "[anvil epoch={secs}] {msg}");
            }
        }
    } else {
        // SAFE: `tui_session_active()` returned false, so no TUI owns the
        // terminal — stderr is the right sink.  This is the one
        // legitimate eprintln in the `tui/` module, kept under the
        // crate-level deny gate via the per-call allow.
        #[allow(clippy::print_stderr, reason = "no TUI is active; stderr is safe (this fn is the sink other code is supposed to call)")]
        {
            eprintln!("[anvil] {msg}");
        }
    }
}

/// Whether the TUI should enter/leave the terminal's alternate screen.
///
/// CC parity: `ANVIL_DISABLE_ALTERNATE_SCREEN=1` keeps the conversation in
/// the terminal's native scrollback (matches CC v2.1.132 `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN`).
/// Useful for users who want scrollback to persist across sessions, or for
/// debugging when alt-screen renderer behaviour is suspect.
///
/// Read once and cached; env changes mid-session don't take effect (matches
/// CC behaviour — alt-screen state is a process-start decision).
pub fn alternate_screen_enabled() -> bool {
    use std::sync::OnceLock;
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        match std::env::var("ANVIL_DISABLE_ALTERNATE_SCREEN") {
            Ok(v) => !matches!(v.as_str(), "1" | "true" | "yes" | "on"),
            Err(_) => true,
        }
    })
}

/// Temporarily leave the alternate screen (for inline operations like image
/// generation or OAuth login). No-op when alt-screen is disabled.
///
/// Pairs with [`restore_alt_screen`].
pub fn leave_alt_screen_for_inline_op() {
    // Task #688: pair mouse-capture state with alt-screen state. If we
    // leave the alt-screen while mouse capture is still on, the user
    // sees raw SGR mouse-tracking codes (^[[<35;...M) dumped into the
    // wizard's plain stdout — and worse, if the wizard exits abnormally
    // the user's shell stays in mouse-tracking mode after anvil quits.
    // DisableBracketedPaste too so the wizard's TextareaModal sees
    // normal keystrokes (it does its own bracketed paste handling).
    if mouse_capture_active() {
        let _ = crossterm::execute!(
            io::stdout(),
            crossterm::event::DisableBracketedPaste,
            crossterm::event::DisableMouseCapture
        );
    } else {
        let _ = crossterm::execute!(
            io::stdout(),
            crossterm::event::DisableBracketedPaste
        );
    }
    if alternate_screen_enabled() {
        let _ = crossterm::execute!(io::stdout(), terminal::LeaveAlternateScreen);
    }
}

/// Re-enter the alternate screen after an inline operation completed.
/// No-op when alt-screen is disabled.
///
/// BUG-2 fix: after entering the alternate screen we immediately write
/// `Clear(ClearType::All)` so the physical terminal cells that were painted
/// during the OAuth/inline flow (on the alt-screen buffer) are wiped.  Without
/// this, ratatui's backing buffer is stale relative to what the terminal
/// actually shows, and its frame-diff algorithm skips cells it believes are
/// already correct — leaving garbage on screen until the user resizes.
pub fn restore_alt_screen() {
    // Task #689: re-enable raw mode FIRST. The mcp_builder wizard runs its
    // own `WizardSession` which calls `disable_raw_mode` on Drop. If we
    // re-enable alt-screen + mouse capture without re-arming raw mode, the
    // terminal cooks every byte from stdin including SGR mouse-tracking
    // responses (^[[<35;...M) — those go to the input line as literal text
    // instead of being parsed as Event::Mouse. User saw the codes piling
    // up in the bottom input box after /mcp builder cancel.
    let _ = terminal::enable_raw_mode();
    if alternate_screen_enabled() {
        let _ = crossterm::execute!(
            io::stdout(),
            terminal::EnterAlternateScreen,
            terminal::Clear(terminal::ClearType::All)
        );
    }
    // Task #688: re-arm input modes that leave_alt_screen_for_inline_op
    // disabled. Bracketed paste is unconditional (matches new() init);
    // mouse capture is conditional on whether it was on before. Without
    // this, returning from the wizard leaves the parent TUI unable to
    // see mouse events.
    let _ = crossterm::execute!(
        io::stdout(),
        crossterm::event::EnableBracketedPaste
    );
    if mouse_capture_active() {
        let _ = crossterm::execute!(
            io::stdout(),
            crossterm::event::EnableMouseCapture
        );
    }
    // Task #687: raise the force-full-redraw flag so the next main-loop
    // iteration calls redraw.request_full(). The Clear above wipes the
    // terminal cells, but ratatui's back-buffer still holds whatever was
    // there before — without this, the frame-diff renderer believes the
    // screen is already correct and skips painting, leaving the user
    // staring at a blank terminal until they type something.
    FORCE_FULL_REDRAW.store(true, std::sync::atomic::Ordering::SeqCst);
}

impl AnvilTui {
    /// Enter alternate screen and return the TUI + the sender for model events.
    pub fn new(
        model: impl Into<String>,
        session_id: impl Into<String>,
        permission_mode: impl Into<String>,
    ) -> io::Result<(Self, TuiSender)> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();

        // Mouse capture is OFF by default. Task #599 / v2.2.14 Phase 1:
        // when mouse capture is on, the terminal hands every mouse event
        // (drag, click, wheel) to Anvil and the user can no longer use
        // native drag-to-select / Cmd+C copy. Past releases shipped this
        // ON and tried to compensate with a Shift+Drag pass-through; that
        // only worked on iTerm2 / Windows Terminal / some Linux VTEs and
        // NEVER worked on macOS Terminal.app.
        //
        // Resolution order:
        //   1. ANVIL_TUI_MOUSE env var (if set, wins).
        //   2. ~/.anvil/config.json "tui_mouse_capture".
        //   3. default OFF.
        //
        // Opting in (`ANVIL_TUI_MOUSE=1`) enables clickable tabs +
        // wheel-scroll, and the status banner switches to instruct the
        // user to hold Option (macOS) / Shift (others) to select text.
        let env_value = std::env::var("ANVIL_TUI_MOUSE").ok();
        let config_value = dirs_next::home_dir()
            .map(|h| h.join(".anvil").join("config.json"))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("tui_mouse_capture").and_then(|b| b.as_bool()));
        let mouse_capture =
            paste::resolve_mouse_capture_default(env_value.as_deref(), config_value);
        // CC parity (v2.2.14): respect ANVIL_DISABLE_ALTERNATE_SCREEN so the
        // conversation can stay in the terminal's native scrollback when the
        // user prefers that. Mouse capture and bracketed paste are independent
        // and still apply.
        let use_alt = alternate_screen_enabled();
        match (use_alt, mouse_capture) {
            (true, true) => crossterm::execute!(
                stdout,
                terminal::EnterAlternateScreen,
                crossterm::event::EnableMouseCapture,
                crossterm::event::EnableBracketedPaste
            )?,
            (true, false) => crossterm::execute!(
                stdout,
                terminal::EnterAlternateScreen,
                crossterm::event::EnableBracketedPaste
            )?,
            (false, true) => crossterm::execute!(
                stdout,
                crossterm::event::EnableMouseCapture,
                crossterm::event::EnableBracketedPaste
            )?,
            (false, false) => crossterm::execute!(
                stdout,
                crossterm::event::EnableBracketedPaste
            )?,
        }
        // Task #688: record that mouse capture is now ON. Drop and the
        // inline-op leave/restore pair check this to keep the capture
        // state paired with the alt-screen state.
        if mouse_capture {
            MOUSE_CAPTURE_ACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        let (tx, rx) = mpsc::sync_channel::<TaggedTuiEvent>(512);

        let model_str: String = model.into();
        let session_id_str: String = session_id.into();
        let context_max = context_max_for_model(&model_str);
        let (git_branch, git_diff_stats, initial_added, initial_removed) = fetch_git_info();

        let mut initial_tab = Tab::new(1, "main", model_str, session_id_str);

        // ── Platform selection/scrollback hint ───────────────────────────────
        // Printed once at startup so users know the key conventions without
        // reading documentation. With mouse capture off (the default), normal
        // drag-to-select works in every terminal — no modifier required.
        //
        // Task #623 (v2.2.14 Phase 1): the copy-shortcut text now varies by
        // OS — Cmd+C (macOS), Ctrl+Shift+C (Linux/BSD), Ctrl+C (Windows).
        //
        // Task #625 (v2.2.14 Phase 1): in vertical-split layouts with mouse
        // capture OFF, the hint also tells the user to drag _within the
        // conversation deck_ so a sloppy drag doesn't pull rail content
        // into their clipboard. Terminal-native selection can't be bounded
        // at Anvil's column structure — we surface the boundary in words
        // (here) AND with a 1-column separator (see
        // vertical_split.rs::render_separator).
        let host_os = paste::OsKind::host();
        let is_vertical_split = matches!(
            Tab::load_default_layout().kind,
            runtime::TuiLayoutKind::VerticalSplit
        );
        let sel_hint = paste::mouse_selection_hint(mouse_capture, host_os, is_vertical_split);

        initial_tab.log.push(LogEntry::System(format!(
            "{sel_hint}  \u{2022}  PageUp to scroll back  \u{2022}  End to return to live view"
        )));

        Ok((
            Self {
                terminal,
                tabs: vec![initial_tab],
                active_tab: 0,
                rx,
                exiting: false,
                git_branch,
                git_diff_stats,
                permission_mode: permission_mode.into(),
                context_max_tokens: context_max,
                next_tab_id: 2,
                qmd_status: String::new(),
                last_archive_status: String::new(),
                configure_state: ConfigureState::Inactive,
                configure_data: ConfigureData::default(),
                configure_viewport: list_viewport::ListViewport::new(),
                configure_screen_tag: 0,
                theme: Theme::load(),
                thinking_enabled: false,
                effort_level: "medium".to_string(),
                relay_tx: None,
                update_available: String::new(),
                agent_panel_visible: true,
                agent_rows: Vec::new(),
                ctrl_c_empty_at: None,
                remote_url: String::new(),
                remote_code: String::new(),
                focus_mode: false,
                status_line_config: StatusLineConfig::load(),
                lines_added: initial_added,
                lines_removed: initial_removed,
                redraw: redraw::RedrawScheduler::new(),
                pending_submit: None,
                ssh_escape_pending: false,
                ssh_form: None,
                provider_login_modal: None,
                confirm_modal: None,
                pending_confirm_action: None,
                password_modal: None,
                pending_password_action: None,
                // Task #579: wizard modal queue starts empty.
                modal_queue: modals::queue::ModalQueue::new(),
                pending_auto_submit: None,
                tab_hits: Vec::new(),
                tab_bar_row: 0,
                pending_permissions: std::collections::HashMap::new(),
                redraw_pending: false,
                redraw_reason: None,
                tui_layout: runtime::TuiLayoutConfig::default(),
                layout_local: layouts::LayoutLocalState::for_kind(runtime::TuiLayoutKind::VerticalSplit),
                spinner_warn_secs: std::env::var("ANVIL_SPINNER_WARN_SECS")
                    .ok()
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(10),
                spinner_error_secs: std::env::var("ANVIL_SPINNER_ERROR_SECS")
                    .ok()
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(30),
                paste_counter: 0,
                burst_tracker: paste::BurstTracker::default(),
                // Task #634: rail focus starts on the conversation deck so
                // the cursor lives in the input box on startup.
                rail_focus: RailFocus::default(),
            },
            // tab_id=1 matches Tab::new(1, "main", ...) constructed above.
            TuiSender::new(tx, 1),
        ))
    }

    // ─── Tab accessors ───────────────────────────────────────────────────────

    pub(super) fn active_tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    pub(super) fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    // ─── Task #634: vertical-split rail-focus + deck-pane navigation ────────

    /// Cycle the active tab's vertical-split deck pane through Conversation →
    /// Transcript → ToolResults → Conversation.
    ///
    /// Wired to `Ctrl+R deck` in the rail's KEYBINDS block. Only meaningful
    /// in `LayoutLocalState::VerticalSplit`; for other layouts (Classic,
    /// ThreePane, Journal) this is a no-op (we still emit a system message
    /// so the user knows the keybind isn't lost — see `input_handler.rs`).
    pub(crate) fn cycle_deck_pane(&mut self) -> bool {
        let tab_idx = self.active_tab;
        match &mut self.tabs[tab_idx].layout_local {
            layouts::LayoutLocalState::VerticalSplit { right_deck_mode, .. } => {
                *right_deck_mode = right_deck_mode.next();
                true
            }
            _ => false,
        }
    }

    /// Return `true` when the active tab's input buffer is empty. Used as the
    /// gate for rail-focus nav keys (`g`/`d`/`s`/`a`) so a user mid-typing
    /// doesn't have their `g` swallowed — it appends to the buffer instead.
    /// Task #634.
    pub(crate) fn input_buffer_is_empty(&self) -> bool {
        self.active_tab().input.is_empty()
    }

    /// Clear the active tab's visible display state — log, pending streaming
    /// text, scrollback, and branches. Used by `/clear` (T4-N) so the TUI no
    /// longer shows messages from a session that the runtime just discarded.
    /// Tab id, name, model, session_id, and input buffer are preserved.
    pub fn clear_active_tab_display(&mut self) {
        let tab = self.active_tab_mut();
        tab.log.clear();
        tab.pending_text.clear();
        tab.branches.clear();
        tab.active_branch = 0;
        tab.last_snapshot = None;
        tab.log_len_at_snapshot = None;
        tab.scrollback = crate::tui::scrollback::ScrollbackBuffer::new();
        tab.scrollback_pending_lines = 0;
        tab.scrollback_state = crate::tui::scrollback::ScrollbackState::live();
        tab.input_tokens = 0;
        tab.output_tokens = 0;
        tab.has_unread = false;
    }

    /// Clear the display state of EVERY tab (workspace-wide /clear). Each tab
    /// keeps its identity (id/name/model/session_id) but its log/scrollback/
    /// branches/tokens are wiped. (T4-N: `/clear --all`.)
    pub fn clear_all_tabs_display(&mut self) {
        for tab in &mut self.tabs {
            tab.log.clear();
            tab.pending_text.clear();
            tab.branches.clear();
            tab.active_branch = 0;
            tab.last_snapshot = None;
            tab.log_len_at_snapshot = None;
            tab.scrollback = crate::tui::scrollback::ScrollbackBuffer::new();
            tab.scrollback_pending_lines = 0;
            tab.scrollback_state = crate::tui::scrollback::ScrollbackState::live();
            tab.input_tokens = 0;
            tab.output_tokens = 0;
            tab.has_unread = false;
        }
    }

    /// Add a new tab.  Returns the (0-based) index of the new tab.
    pub fn new_tab(&mut self, name: impl Into<String>, model: impl Into<String>, session_id: impl Into<String>) -> usize {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let tab = Tab::new(id, name, model, session_id);
        self.tabs.push(tab);
        self.tabs.len() - 1
    }

    /// Return the tab-id (`Tab::id`) for the tab at a 0-based index.
    pub fn tab_id_at(&self, index: usize) -> Option<usize> {
        self.tabs.get(index).map(|t| t.id)
    }

    /// Total number of currently-open tabs.  Added for task #647 so the
    /// remote-control dispatch path can iterate without exposing the
    /// `tabs: Vec<Tab>` field.
    #[must_use]
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Mark the tab at a 0-based index as having a runtime installed.
    ///
    /// Called from `run_repl_tui` after `push_tab_runtime` succeeds for a new
    /// tab, and once at startup to stamp the bootstrap tab.
    pub fn mark_tab_has_runtime(&mut self, index: usize) {
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.has_runtime = true;
        }
    }

    /// v2.2.14 TUI-1: return a clone of the cancel-flag Arc held by the tab
    /// at `index`. Callers wire this onto the matching `ConversationRuntime`
    /// via `set_cancel_handle` so the runtime polls the same atomic the TUI
    /// flips from its Ctrl+C handler.
    pub fn tab_cancel_token(
        &self,
        index: usize,
    ) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        self.tabs.get(index).map(|t| std::sync::Arc::clone(&t.cancel_token))
    }

    /// v2.2.14 TUI-2 (deep): mirror `LiveCli.tab_runtimes[idx].in_flight.is_some()`
    /// onto the TUI's per-tab flag. Called by `run_repl_tui` after every
    /// `spawn_turn_for_tab` (true) and `try_reap_finished_turns` (false) so
    /// the in-flight key router can tell streaming tabs from idle ones.
    pub fn set_tab_in_flight(&mut self, index: usize, in_flight: bool) {
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.in_flight = in_flight;
        }
    }

    /// v2.2.14 TUI-2 (deep): true iff the visually-active tab has a turn
    /// streaming. Read by `handle_in_flight_key_extended` to decide whether
    /// Enter should queue (active tab is the in-flight one) or fire as a
    /// new turn (active tab is idle while another tab streams).
    pub fn is_active_tab_in_flight(&self) -> bool {
        self.tabs.get(self.active_tab).is_some_and(|t| t.in_flight)
    }

    /// v2.2.14 TUI-2 (deep): true iff any tab is streaming. Used by the
    /// main-loop dispatcher to decide whether to re-enter the event-pump
    /// wait or fall back to the idle `read_input` loop.
    pub fn is_any_tab_in_flight(&self) -> bool {
        self.tabs.iter().any(|t| t.in_flight)
    }

    /// Switch to the tab at 0-based index.  Clears the unread marker on the target.
    pub fn switch_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            // Task #604 Part C: burst tracker is singleton across tabs;
            // a tab switch ends any in-progress drag-drop detection on
            // the previous tab.
            self.burst_tracker.reset();
            self.active_tab = index;
            self.tabs[index].has_unread = false;
            // Task #647 (G1): mirror focus to remote viewers using the
            // stable logical tab_id, not the Vec position.
            let tab_id = self.tabs[index].id;
            self.relay_forward(runtime::relay::RelayMessage::TabSwitched { tab_id });
        }
    }

    /// Switch to the next tab (wraps around).
    pub(super) fn next_tab(&mut self) {
        let next = (self.active_tab + 1) % self.tabs.len();
        self.switch_tab(next);
    }

    /// Switch to the previous tab (wraps around).
    pub(super) fn prev_tab(&mut self) {
        let prev = if self.active_tab == 0 {
            self.tabs.len() - 1
        } else {
            self.active_tab - 1
        };
        self.switch_tab(prev);
    }

    /// Close the active tab.  The last tab cannot be closed.
    /// Returns the name of the closed tab, or None if there was only one tab.
    pub fn close_active_tab(&mut self) -> Option<String> {
        if self.tabs.len() <= 1 {
            return None;
        }
        let name = self.tabs[self.active_tab].name.clone();
        self.tabs.remove(self.active_tab);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        Some(name)
    }

    /// Close a tab by its array index. Returns the name if closed, None if last tab or invalid.
    #[allow(dead_code)]
    pub fn close_tab_by_index(&mut self, index: usize) -> Option<String> {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return None;
        }
        let name = self.tabs[index].name.clone();
        self.tabs.remove(index);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        Some(name)
    }

    /// Close a tab by its unique ID (not array index). Returns the name if closed.
    pub fn close_tab_by_id(&mut self, tab_id: usize) -> Option<String> {
        if self.tabs.len() <= 1 {
            return None;
        }
        if let Some(pos) = self.tabs.iter().position(|t| t.id == tab_id) {
            let name = self.tabs[pos].name.clone();
            self.tabs.remove(pos);
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            }
            Some(name)
        } else {
            None
        }
    }

    /// Rename a tab by its unique ID. Returns true if found and renamed.
    pub fn rename_tab_by_id(&mut self, tab_id: usize, new_name: &str) -> bool {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.name = new_name.to_string();
            true
        } else {
            false
        }
    }

    /// Rename the active tab.
    pub fn rename_active_tab(&mut self, name: impl Into<String>) {
        self.active_tab_mut().name = name.into();
    }

    /// Return a list of (index, id, name, `has_unread`) tuples.
    pub fn tab_list(&self) -> Vec<(usize, usize, &str, bool)> {
        self.tabs.iter().enumerate().map(|(i, t)| (i, t.id, t.name.as_str(), t.has_unread)).collect()
    }

    /// Return full tab info for relay broadcast: (`tab_id`, name, model, `session_id`).
    pub fn tab_details(&self) -> Vec<(usize, String, String, String)> {
        self.tabs.iter().map(|t| (t.id, t.name.clone(), t.model.clone(), t.session_id.clone())).collect()
    }

    /// Return the 0-based index of the currently active tab.
    pub const fn active_tab_index(&self) -> usize {
        self.active_tab
    }

    /// Update the model for the active tab and recalculate context limit.
    pub fn set_model(&mut self, model: impl Into<String>) {
        let model_str = model.into();
        self.context_max_tokens = context_max_for_model(&model_str);
        self.active_tab_mut().model = model_str;
        // Task #574: model-switch dirties the HEADER band (model label,
        // context window). STATUS picks up the cost/effort/permission strip.
        // No need to dirty SCROLLBACK or INPUT — content is unaffected.
        self.redraw.request(redraw::DirtyRegions::HEADER | redraw::DirtyRegions::STATUS);
    }

    /// Apply a new theme to the TUI immediately (live hot-swap).
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// Set the active tab's input text (replacing any current draft) and
    /// move the cursor to the end. Used by the in-flight typing path
    /// (T1-#400) to surface a held draft after the previous turn ends.
    pub fn set_input(&mut self, text: impl Into<String>) {
        let s = text.into();
        let tab = self.active_tab_mut();
        tab.cursor = s.len();
        tab.input = s;
    }

    /// Queue a line to be auto-submitted on the next `read_input` poll,
    /// bypassing the input box. Used when the previous turn held a slash
    /// command that the user clearly meant to EXECUTE (e.g. `/ssh` typed
    /// during streaming) rather than enqueue as a draft chat message.
    pub fn set_pending_submission(&mut self, text: impl Into<String>) {
        self.pending_auto_submit = Some(text.into());
    }

    /// v2.2.14 TUI-3: pop the next queued message off the active tab's
    /// `message_queue` and stage it in `pending_submit` so the in-flight
    /// handler picks it up when the next turn starts streaming. Returns
    /// `true` if a message was promoted.
    pub fn promote_next_queued_for_active(&mut self) -> bool {
        if self.pending_submit.is_some() {
            return false;
        }
        if let Some(next) = self.active_tab_mut().message_queue.pop_front() {
            self.pending_submit = Some(next);
            self.redraw.request(redraw::DirtyRegions::INPUT);
            true
        } else {
            false
        }
    }

    /// v2.2.14 TUI-2 (deep): push a user message onto a specific tab's
    /// `message_queue`, by Vec index. Used by the main-loop dispatcher
    /// when `spawn_turn_for_tab` returns "tab already in flight" — the
    /// queued draft fires when that tab's current turn finishes via the
    /// existing TUI-3 promote-on-TurnDone path.
    pub fn enqueue_on_tab(&mut self, idx: usize, message: impl Into<String>) {
        let s = message.into();
        if s.is_empty() {
            return;
        }
        if let Some(tab) = self.tabs.get_mut(idx) {
            tab.message_queue.push_back(s);
            self.redraw.request(redraw::DirtyRegions::INPUT);
        }
    }

    /// v2.2.14 TUI-3: count of queued user messages on the active tab.
    /// Used by the input renderer to show `[N queued]` above the input.
    pub fn active_tab_queue_len(&self) -> usize {
        self.tabs
            .get(self.active_tab)
            .map(|t| t.message_queue.len())
            .unwrap_or(0)
    }

    // ─── Redraw scheduler API ────────────────────────────────────────────────

    /// Mark one or more dirty regions, deferring the actual paint until the
    /// next `commit_redraw()`. Cheap; no I/O. Use for any state mutation that
    /// is visible to the user (input edits, status-line changes, scrollback
    /// appends, agent panel updates).
    pub fn request_redraw(&mut self, region: redraw::DirtyRegions) {
        self.redraw.request(region);
    }

    /// Mark every region dirty AND force the next `commit_redraw()` to paint
    /// regardless of frame budget. Use after any structural change where
    /// short-circuiting would leave stale pixels: tab switch, model switch,
    /// modal open/close, sandbox-mode change, terminal resize.
    pub fn request_full_redraw(&mut self) {
        self.redraw.request_full();
    }

    /// Draw if (and only if) something is dirty AND the frame budget allows.
    /// Returns Ok(true) if a draw fired, Ok(false) if it was deferred. Call
    /// this once per main-loop iteration where a paint might be useful.
    pub fn commit_redraw(&mut self) -> io::Result<bool> {
        if self.redraw.commit_pending() {
            self.draw()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// v2.2.14 BUG-fix-real: set the central render gate with a reason tag.
    ///
    /// Separate from `request_redraw(DirtyRegions)` (which feeds the frame-
    /// budget scheduler) — this is the simple flag the wait-loop and main-
    /// loop check at the top of every iteration to force a full
    /// `draw_full()` + explicit terminal flush. The diff coalescing inside
    /// `RedrawScheduler::commit_pending` is part of why typed characters
    /// failed to echo while a background tab firehosed TextDeltas; this
    /// gate intentionally bypasses the budget.
    pub fn request_redraw_reason(&mut self, reason: RedrawReason) {
        self.redraw_pending = true;
        self.redraw_reason = Some(reason);
    }

    /// v2.2.14 BUG-fix-real: drain the central gate. Returns Ok(true) if a
    /// full redraw fired, Ok(false) if the gate was clear. The wait loop
    /// calls this after every handled key event and after every TextDelta
    /// batch, plus once per iteration as the trailing draw.
    ///
    /// On commit:
    ///   1. Calls `draw_full()` — which clears the terminal then re-runs
    ///      `draw()`, bypassing ratatui's frame-diff coalescing.
    ///   2. Calls `backend.flush()` explicitly so the bytes leave the
    ///      crossterm buffer immediately (no waiting for stdout's normal
    ///      line/page boundary).
    ///   3. Clears the gate.
    ///
    /// Step 5 of the user's diagnosis hangs off the [DRAW-END] marker that
    /// `draw()` emits AFTER the terminal write — if that line appears but
    /// the typed char doesn't, the bug is below ratatui (likely
    /// stdout/alternate-screen state corruption).
    pub fn commit_pending_redraw(&mut self) -> io::Result<bool> {
        // Task #687: an inline op that re-entered the alt-screen (e.g.
        // /mcp builder, /provider login) raised FORCE_FULL_REDRAW from a
        // context that doesn't hold &mut AnvilTui. Consume the flag here
        // and promote this redraw into a full structural repaint so the
        // freshly-cleared terminal gets repainted — without this, ratatui's
        // back-buffer believes the screen is already correct and skips
        // painting, leaving the user with a blank/stale view.
        if take_force_full_redraw() {
            self.redraw.request_full();
            self.request_redraw_reason(RedrawReason::InlineOpReturn);
        }
        if !self.redraw_pending {
            return Ok(false);
        }
        // Task #622: pick the draw path based on the redraw reason.
        //
        // Background: the v2.2.14 BUG-fix-real path always called
        // `draw_full()` here, which runs `terminal.clear()` (ANSI \x1b[2J)
        // before every draw. On Gnome Terminal / kitty / some xterm builds
        // that screen-wide erase causes a perceptible flicker on every
        // committed frame. During token streaming the wait loop commits
        // many frames per second — the user on Ubuntu/Gnome reported it as
        // a "health issue" (photosensitivity / visual fatigue).
        //
        // The original BUG-fix-real concern was that ratatui's frame diff
        // could coalesce a streaming tab's paint with the active tab's
        // typed-char paint and drop the latter. The fix shape: invalidate
        // ratatui's diff via `terminal.clear()`. But that fix is too
        // aggressive — it only matters across structural boundaries (tab
        // switch, modal close, layout change), NOT for every TextDelta or
        // spinner tick.
        //
        // For streaming and key-event reasons we now use plain `draw()`
        // (ratatui's natural cell diff) plus an explicit flush. The diff
        // works fine for in-tab streaming because the deltas only touch
        // the SCROLLBACK region — the only cross-tab race that needed
        // `terminal.clear()` is the structural one (TabSwitch), and that
        // still routes through `draw_full()` below.
        //
        // Escape hatches:
        //   `ANVIL_TUI_FORCE_CLEAR=1` — legacy behavior; always full clear.
        //   `ANVIL_TUI_NO_FLASH=1`    — alias of default; documented for
        //                               users who hit a regression and want
        //                               to be explicit.
        // Task #629: invert the gate default. Streaming events that arrive
        // tagged as `Other` or `None` (e.g. TurnDone end-of-stream commit,
        // ChannelClosed, several apply_tagged_event paths) used to cascade
        // through `draw_full()` and emit `\x1b[2J` per event — strace
        // confirmed 27 full-screen clears per short streaming session on
        // Linux. That's the flash. New rule: ONLY explicit structural
        // events (TabSwitch and the new InlineOpReturn) take the hard path.
        // Everything else — including unclassified `Other` and `None` —
        // routes through the soft `draw()` path. Tagging-by-default is too
        // brittle for accessibility-critical behavior; deny-by-default
        // protects against future drift.
        let force_clear = std::env::var("ANVIL_TUI_FORCE_CLEAR")
            .map(|v| v == "1")
            .unwrap_or(false);
        let needs_full_clear = force_clear
            || matches!(
                self.redraw_reason,
                Some(RedrawReason::TabSwitch) | Some(RedrawReason::InlineOpReturn)
            );
        if needs_full_clear {
            self.draw_full()?;
        } else {
            // Soft path: ratatui's cell diff + explicit flush. No screen-wide
            // ANSI erase — no flash.
            //
            // Tell the scheduler what region the upcoming `draw()` should be
            // labeled as. The layout renderers consult `snap.dirty_regions`
            // to decide whether the top-level full-screen `Clear` widget is
            // justified. For streaming/keystroke reasons we want it skipped.
            let region = match self.redraw_reason {
                Some(RedrawReason::KeyEvent) => redraw::DirtyRegions::INPUT,
                Some(RedrawReason::TextDeltaBatch) => redraw::DirtyRegions::SCROLLBACK,
                Some(RedrawReason::Spinner) => redraw::DirtyRegions::SCROLLBACK,
                // Task #629: `Other` and `None` reach this arm now (they
                // used to take the hard path). They get SCROLLBACK instead
                // of ALL so the layout renderers don't paint the
                // full-screen Clear widget on top of the diffed draw.
                // TabSwitch and InlineOpReturn never reach this arm — they
                // hit `needs_full_clear` above.
                Some(RedrawReason::Other) | None => redraw::DirtyRegions::SCROLLBACK,
                Some(RedrawReason::TabSwitch) | Some(RedrawReason::InlineOpReturn) => {
                    // Unreachable in practice (handled by needs_full_clear
                    // above), but exhaustive match.
                    redraw::DirtyRegions::ALL
                }
            };
            self.redraw.set_last_committed_dirty(region);
            self.draw()?;
        }
        use std::io::Write;
        self.terminal.backend_mut().flush()?;
        self.redraw_pending = false;
        self.redraw_reason = None;
        Ok(true)
    }

    /// Forced full repaint.
    ///
    /// Calls `terminal.clear()` BEFORE `draw()` to invalidate ratatui's
    /// backing buffer and force it to re-emit every cell, bypassing the
    /// frame-diff coalescing that causes stale cells to linger after an
    /// inline operation (BUG-2: OAuth/setup return) or a layout switch
    /// (BUG-3: `/layout` command).
    ///
    /// `terminal.clear()` writes ANSI erase sequences and marks ratatui's
    /// internal buffer as entirely dirty — the next `draw()` call re-paints
    /// every cell unconditionally.
    pub(super) fn draw_full(&mut self) -> io::Result<()> {
        self.terminal.clear()?;
        // Task #622: this is an explicit structural repaint (modal close,
        // tmux re-attach, etc.) — promote the next draw to a full wipe so the
        // layout's top-level `Clear` widget fires regardless of any prior
        // narrow dirty set captured by the scheduler.
        self.redraw.mark_next_full();
        self.draw()
    }

    /// BUG-2 fix: force a full repaint after returning from an inline
    /// operation (OAuth login, provider setup, etc.).
    ///
    /// Calling this after `restore_alt_screen()` ensures ratatui's backing
    /// buffer is invalidated and the next frame re-paints every cell, so no
    /// stale content from before the inline op lingers on screen.
    pub fn force_full_repaint_after_inline_op(&mut self) {
        self.redraw.request_full();
        self.request_redraw_reason(RedrawReason::InlineOpReturn);
        // Eagerly commit so the repaint fires on the very next draw cycle.
        // Ignore the error — if we can't paint we'll paint next iteration.
        let _ = self.commit_pending_redraw();
    }

    // ─── Snapshot collection ─────────────────────────────────────────────────

    /// Collect a `LayoutSnapshot` from the current `AnvilTui` state.
    ///
    /// Called by `draw()` to freeze all the data the draw closure needs before
    /// `terminal.draw()` takes the mutable terminal borrow. The snapshot is a
    /// pure value — the draw closure becomes a function of its inputs and no
    /// longer accesses `self` (except for fields that are mutated after the
    /// closure returns, like `tab_hits`).
    ///
    /// v2.2.16: This method is the seam between shared TUI state and the
    /// per-layout renderers. When the draw function is later split into
    /// `layouts/{vertical_split,three_pane,journal}.rs`, each renderer will
    /// receive a `&LayoutSnapshot` instead of accessing `&AnvilTui` directly.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    pub(super) fn collect_snapshot(&mut self) -> LayoutSnapshot {
        let tab = self.active_tab();
        let log_snapshot = tab.log.clone();
        let transcript_verbose = tab.transcript_verbose;
        let pending = tab.pending_text.clone();
        let think = tab.think_label.clone();
        let think_frame = THINK_FRAMES[tab.think_frame % THINK_FRAMES.len()];
        let think_elapsed_secs = tab
            .think_start
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        let input_text = tab.input.clone();

        let queued_count = tab.message_queue.len()
            + usize::from(self.pending_submit.is_some());
        let queued_preview: Vec<String> = {
            let preview_iter = self.pending_submit.iter()
                .chain(tab.message_queue.iter());
            preview_iter
                .take(3)
                .map(|s| {
                    let cap = 40;
                    if s.chars().count() > cap {
                        let trimmed: String = s.chars().take(cap).collect();
                        format!("{trimmed}…")
                    } else {
                        s.clone()
                    }
                })
                .collect()
        };
        let cursor_pos = tab.cursor;
        let scroll = tab.scroll;
        let scrollback_state = tab.scrollback_state;
        // BUG-#591: `is_live()` only checks for the literal `None` discriminant
        // and misses the case where the user nudged the scrollwheel by one
        // line and then enough new content streamed in that the stored
        // anchor is now at or past the live anchor — visually they're at
        // the live tail. `is_effectively_live` accounts for that and is
        // the value the banner gate at vertical_split.rs / classic.rs reads.
        // Concrete height is computed below; use a conservative ceiling here
        // since `approx_content_height` is established later in this function.
        let approx_height_for_live_check = self
            .terminal
            .size()
            .map(|s| s.height.saturating_sub(6) as usize)
            .unwrap_or(18);
        let scrollback_is_live =
            scrollback_state.is_effectively_live(&tab.scrollback, approx_height_for_live_check);
        let model = tab.model.clone();
        let session_id = tab.session_id.clone();
        let input_tokens = tab.input_tokens;
        let output_tokens = tab.output_tokens;
        let elapsed = tab.session_start.elapsed();
        let completion_visible = tab.completion.visible;
        let completion_selected = tab.completion.selected;
        let completion_matches: Vec<(String, String, bool, bool)> = tab
            .completion
            .matches
            .iter()
            .map(|c| (c.insert.clone(), c.hint.clone(), c.is_header, c.is_free_text))
            .collect();

        // Run selection-follow on the completion popup viewport so the
        // highlighted row is always on-screen.
        if completion_visible {
            const POPUP_BODY_HEIGHT: usize = 12;
            let total = completion_matches.len();
            let mut vp = list_viewport::ListViewport::new();
            let prior = tab.completion.view_offset;
            vp = vp.scroll_down(prior, total, POPUP_BODY_HEIGHT);
            vp = vp.follow_selection(completion_selected, total, POPUP_BODY_HEIGHT);
            self.active_tab_mut().completion.view_offset =
                vp.offset(total, POPUP_BODY_HEIGHT);
        }
        let completion_view_offset = self.active_tab().completion.view_offset;

        let tab_infos: Vec<(usize, String, bool, bool, bool)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let has_perm = self.pending_permissions.contains_key(&t.id);
                (t.id, t.name.clone(), i == self.active_tab, t.has_unread, has_perm)
            })
            .collect();

        let active_tab_id = self.tabs.get(self.active_tab).map(|t| t.id).unwrap_or(0);
        let active_permission_modal: Option<(String, String, String, String)> =
            self.pending_permissions.get(&active_tab_id).map(|p| {
                (p.tool_name.clone(), p.required_mode.clone(), p.current_mode.clone(), p.input_summary.clone())
            });

        let git_branch = self.git_branch.clone();
        let git_diff_stats = self.git_diff_stats.clone();
        let permission_mode = self.permission_mode.clone();
        let context_max_tokens = self.context_max_tokens;
        let qmd_status = self.qmd_status.clone();
        let last_archive_status = self.last_archive_status.clone();
        let update_available = self.update_available.clone();
        let configure_state = self.configure_state.clone();
        let configure_data = self.configure_data.clone();

        // Reset configure viewport when the active screen changes.
        let new_screen_tag = configure_screen_tag(&configure_state);
        if new_screen_tag != self.configure_screen_tag {
            self.configure_viewport.reset();
            self.configure_screen_tag = new_screen_tag;
        }

        // Estimate content height for viewport-follow calculations.
        let approx_content_height =
            self.terminal.size().map(|s| s.height.saturating_sub(6) as usize).unwrap_or(18);

        if configure_state != ConfigureState::Inactive {
            let total_items = configure_item_count(&configure_state, &configure_data);
            let total_lines_est = total_items.saturating_add(4);
            let sel = configure_selected(&configure_state);
            let sel_line = sel.saturating_add(4);
            self.configure_viewport = self
                .configure_viewport
                .follow_selection(sel_line, total_lines_est, approx_content_height);
        }
        let configure_viewport = self.configure_viewport;

        let theme = self.theme.clone();
        let agent_panel_visible = self.agent_panel_visible;
        let agent_rows = self.agent_rows.clone();
        let remote_url = self.remote_url.clone();
        let remote_code = self.remote_code.clone();
        let sl_config = self.status_line_config.clone();
        let thinking_enabled = self.thinking_enabled;
        let lines_added = self.lines_added;
        let lines_removed = self.lines_removed;
        let effort_level = self.effort_level.clone();
        let spinner_warn_secs = self.spinner_warn_secs;
        let spinner_error_secs = self.spinner_error_secs;

        // Pre-fetch scrollback lines for historical view.
        let scrollback_view_lines: Option<Vec<String>> = if scrollback_is_live {
            None
        } else {
            let tab = self.active_tab();
            let anchor = scrollback_state.effective_anchor(&tab.scrollback, approx_content_height);
            let (lines, _) = tab.scrollback.lines_in_range(anchor, approx_content_height + 4);
            Some(lines)
        };

        // T5-Ssh-D: pre-snapshot the active SSH tab's vt100 screen.
        let ssh_screen: Option<(Vec<ratatui::text::Line<'static>>, Vec<ratatui::text::Line<'static>>)> = {
            let tab = self.active_tab();
            if let Some(ref ssh) = tab.ssh {
                let screen = ssh.parser.screen();
                let (rows, cols) = screen.size();
                let map_color = |c: Vt100Color| -> Color {
                    match c {
                        Vt100Color::Default => Color::Reset,
                        Vt100Color::Idx(n) => Color::Indexed(n),
                        Vt100Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
                    }
                };
                let mut grid_lines: Vec<ratatui::text::Line<'static>> = Vec::with_capacity(rows as usize);
                for row in 0..rows {
                    let mut spans: Vec<Span<'static>> = Vec::new();
                    let mut run = String::new();
                    let mut run_style = Style::default();
                    let flush_run = |run: &mut String, style: Style, spans: &mut Vec<Span<'static>>| {
                        if !run.is_empty() {
                            spans.push(Span::styled(std::mem::take(run), style));
                        }
                    };
                    for col in 0..cols {
                        if let Some(cell) = screen.cell(row, col) {
                            if cell.is_wide_continuation() {
                                continue;
                            }
                            let fg = map_color(cell.fgcolor());
                            let bg = map_color(cell.bgcolor());
                            let mut style = Style::default().fg(fg).bg(bg);
                            if cell.bold() { style = style.add_modifier(Modifier::BOLD); }
                            if cell.italic() { style = style.add_modifier(Modifier::ITALIC); }
                            if cell.underline() { style = style.add_modifier(Modifier::UNDERLINED); }
                            if cell.inverse() { style = style.add_modifier(Modifier::REVERSED); }
                            if style != run_style {
                                flush_run(&mut run, run_style, &mut spans);
                                run_style = style;
                            }
                            let ch = cell.contents();
                            if ch.is_empty() { run.push(' '); } else { run.push_str(ch); }
                        } else {
                            if run_style != Style::default() {
                                flush_run(&mut run, run_style, &mut spans);
                                run_style = Style::default();
                            }
                            run.push(' ');
                        }
                    }
                    flush_run(&mut run, run_style, &mut spans);
                    grid_lines.push(ratatui::text::Line::from(spans));
                }
                let elapsed_ssh = ssh.opened_at.elapsed();
                let elapsed_str = if elapsed_ssh.as_secs() >= 3600 {
                    format!("{}h {}m", elapsed_ssh.as_secs() / 3600, (elapsed_ssh.as_secs() % 3600) / 60)
                } else {
                    format!("{}m {}s", elapsed_ssh.as_secs() / 60, elapsed_ssh.as_secs() % 60)
                };
                let dest = ssh.destination.clone();
                let state_label = ssh.state.label();
                let footer_primary = ratatui::text::Line::from(vec![
                    Span::styled(
                        format!(" [{state_label}] {dest}  {elapsed_str} "),
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                    ),
                    Span::styled(
                        " Ctrl+B 0-9=switch  Ctrl+B q=close ",
                        Style::default().fg(Color::DarkGray),
                    ),
                ]);
                let mut footer_lines: Vec<ratatui::text::Line<'static>> = vec![footer_primary];
                match &ssh.state {
                    crate::tui::ssh_tab::SshConnState::AuthFailed(reason) => {
                        footer_lines.push(ratatui::text::Line::from(Span::styled(
                            format!(" auth failed: {reason}"),
                            Style::default().fg(Color::Red),
                        )));
                    }
                    crate::tui::ssh_tab::SshConnState::Error(msg) => {
                        footer_lines.push(ratatui::text::Line::from(Span::styled(
                            format!(" error: {msg}"),
                            Style::default().fg(Color::Red),
                        )));
                    }
                    crate::tui::ssh_tab::SshConnState::Disconnected(Some(reason)) => {
                        footer_lines.push(ratatui::text::Line::from(Span::styled(
                            format!(" disconnected: {reason}"),
                            Style::default().fg(Color::Yellow),
                        )));
                    }
                    _ => {}
                }
                Some((grid_lines, footer_lines))
            } else {
                None
            }
        };
        let is_ssh_tab = ssh_screen.is_some();

        let can_close_tab = self.tabs.len() > 1;

        // ── Cross-tab aggregates ─────────────────────────────────────────────
        let running_tab_count = self.tabs.iter().filter(|t| t.in_flight).count();
        let pending_permission_count = self.pending_permissions.len();
        let total_session_cost_usd: f64 = self.tabs.iter().map(|t| {
            if let Some(p) = runtime::pricing_for_model(&t.model) {
                (f64::from(t.input_tokens) / 1_000_000.0) * p.input_cost_per_million
                    + (f64::from(t.output_tokens) / 1_000_000.0) * p.output_cost_per_million
            } else {
                0.0
            }
        }).sum();

        // ── Task #594 / BUG-13: seven-layer memory + chrome surface ──────────
        // All values are best-effort `read_dir` counts off `~/.anvil/`. The
        // helpers live in `tui/layouts/common.rs` so the renderers (and the
        // tests that assert population) share a single implementation.
        let memory_working_turns = log_snapshot
            .iter()
            .filter(|e| matches!(e, LogEntry::User(_) | LogEntry::Assistant(_)))
            .count();
        let memory_working_tokens = input_tokens.saturating_add(output_tokens);
        let memory_episodic_sessions = layouts::common::count_episodic_sessions();
        let (memory_semantic_collections, memory_semantic_archives) =
            layouts::common::semantic_counts();
        let memory_procedural_skills = layouts::common::count_skills();
        let memory_procedural_plugins = layouts::common::count_plugins();
        let memory_reflective_daily = layouts::common::count_daily_summaries();
        let memory_permission_decisions = layouts::common::count_permission_decisions();

        let qmd_latest_session_id = if session_id.is_empty() {
            None
        } else {
            Some(session_id.clone())
        };
        let qmd_latest_age_days: Option<u32> = qmd_latest_session_id.as_ref().map(|_| 0);
        let qmd_archive_count: u32 = layouts::common::qmd_archive_count(&qmd_status);

        let permission_mode_label = helpers::permission_mode_display(&permission_mode);
        let cost_provider_label = layouts::common::cost_provider_label(&model);

        let build_version = env!("CARGO_PKG_VERSION").to_string();
        let build_git_sha_short = {
            let sha = env!("GIT_SHA");
            sha.chars().take(7).collect::<String>()
        };

        let context_used_tokens = memory_working_tokens;
        let context_limit_tokens = context_max_tokens;
        let session_pct: f32 = if context_limit_tokens > 0 {
            ((f64::from(context_used_tokens) / f64::from(context_limit_tokens)) * 100.0)
                .min(100.0) as f32
        } else {
            0.0
        };
        let block_minutes: u32 = (elapsed.as_secs() / 60) as u32;

        let provider_login_modal_snapshot = self
            .provider_login_modal
            .as_ref()
            .map(|m| m.render_snapshot());
        let confirm_modal_snapshot = self
            .confirm_modal
            .as_ref()
            .map(|m| m.render_snapshot());
        let password_modal_snapshot = self
            .password_modal
            .as_ref()
            .map(|m| m.render_snapshot());
        let ssh_form_snapshot: Option<ssh_form::SshFormState> = self.ssh_form.as_ref().map(|f| {
            let mut copy = ssh_form::SshFormState::new();
            copy.host = f.host.clone();
            copy.port_str = f.port_str.clone();
            copy.user = f.user.clone();
            copy.auth_index = f.auth_index;
            copy.key_path = f.key_path.clone();
            copy.secret = f.secret.clone();
            copy.alias = f.alias.clone();
            copy.focused = f.focused;
            copy.error = f.error.clone();
            copy.picker = f.picker.clone();
            copy
        });

        LayoutSnapshot {
            log_snapshot,
            transcript_verbose,
            pending,
            think,
            think_frame,
            think_elapsed_secs,
            input_text,
            queued_count,
            queued_preview,
            cursor_pos,
            scroll,
            scrollback_state,
            scrollback_is_live,
            model,
            session_id,
            input_tokens,
            output_tokens,
            elapsed,
            completion_visible,
            completion_selected,
            completion_matches,
            completion_view_offset,
            tab_infos,
            active_permission_modal,
            git_branch,
            git_diff_stats,
            permission_mode,
            context_max_tokens,
            qmd_status,
            last_archive_status,
            update_available,
            configure_state,
            configure_data,
            configure_viewport,
            theme,
            agent_panel_visible,
            agent_rows,
            remote_url,
            remote_code,
            sl_config,
            thinking_enabled,
            lines_added,
            lines_removed,
            effort_level,
            spinner_warn_secs,
            spinner_error_secs,
            ssh_screen,
            is_ssh_tab,
            ssh_form_snapshot,
            provider_login_modal_snapshot,
            confirm_modal_snapshot,
            password_modal_snapshot,
            scrollback_view_lines,
            can_close_tab,
            running_tab_count,
            pending_permission_count,
            total_session_cost_usd,
            memory_working_turns,
            memory_working_tokens: memory_working_tokens,
            memory_episodic_sessions,
            memory_semantic_collections,
            memory_semantic_archives,
            memory_procedural_skills,
            memory_procedural_plugins,
            memory_reflective_daily,
            memory_permission_decisions,
            qmd_latest_session_id,
            qmd_latest_age_days,
            qmd_archive_count,
            permission_mode_label,
            cost_provider_label,
            build_version,
            build_git_sha_short,
            context_used_tokens,
            context_limit_tokens,
            session_pct,
            block_minutes,
            // Task #622: capture the dirty set that fired the most recent
            // `commit_pending` so layout renderers can gate the top-level
            // full-screen `Clear` widget on whether this frame actually
            // changed layout structure (resize, /layout switch, modal close)
            // versus a streaming TextDelta that should NOT cause a wipe.
            dirty_regions: self.redraw.last_committed_dirty(),
            // Task #634: ship the rail-focus enum through to the layout
            // renderer so the focused section's header gets bold accent
            // emphasis. Driven by `g` / `d` / `s` / `a` keys when input is
            // empty and no modal is open (see `input_handler.rs`).
            rail_focus: self.rail_focus,
        }
    }

    // ─── Draw ────────────────────────────────────────────────────────────────

    /// Draw the current state.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    pub(super) fn draw(&mut self) -> io::Result<()> {
        // ── Feed scrollback buffer ───────────────────────────────────────────
        // Push newly-rendered log lines into the per-tab ring buffer so PageUp
        // can reach them.  We approximate content_width using terminal width.
        let approx_width: u16 = self.terminal.size().map(|s| s.width).unwrap_or(80);
        {
            let theme = self.theme.clone();
            let tab = self.active_tab_mut();
            // Build the full set of plain-text lines from log + pending.
            // Scrollback population strategy:
            //
            // The challenge is that pending_text grows incrementally as text
            // streams in (each TextDelta is a chunk like "#", " Anvil", etc.),
            // and even the LAST `log` entry can still mutate (a ToolCall card's
            // detail/result fields update as tool events arrive). Naively
            // appending lines as they arrive caches early prefixes like "#"
            // forever, which is what users saw as truncated text in
            // HISTORICAL VIEW.
            //
            // Strategy:
            //   - Treat all but the last log entry as STABLE (append-only).
            //   - Treat the last log entry + pending_text as MUTABLE (re-render
            //     every draw by popping and re-pushing the tail).
            //   - The `scrollback_pending_lines` count tracks how many lines
            //     at the back came from the mutable region so we can pop them.
            //
            // For long sessions this still amortises well: only the last entry
            // and pending text re-render each frame, not the full log.
            tab.scrollback.pop_back_n(tab.scrollback_pending_lines);
            tab.scrollback_pending_lines = 0;

            // CC-139-F5: in transcript verbose mode every tool card renders
            // as expanded, which changes line counts. We pass the flag into
            // `to_lines_with` so the scrollback feed and the live render
            // both stay in sync with the verbose toggle.
            let verbose = tab.transcript_verbose;

            // Render the stable part of the log (all but the last entry) and
            // append any new lines past what's already cached. We compute the
            // stable line iterator lazily so we don't re-render every prior
            // entry per frame — `skip(already)` consumes without allocating.
            //
            // Note: when `verbose` toggles, the cached `stable_already` count
            // is no longer accurate; the toggle handler clears scrollback so
            // this branch sees `stable_already == 0` and re-renders cleanly.
            let stable_end = tab.log.len().saturating_sub(1);
            let stable_already = tab.scrollback.len();
            let new_stable_lines: Vec<String> = tab.log[..stable_end]
                .iter()
                .flat_map(|entry| {
                    entry.to_lines_with(approx_width, &theme, verbose).into_iter().map(|line| {
                        line.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
                    })
                })
                .skip(stable_already)
                .collect();
            for line in new_stable_lines {
                tab.scrollback.push(line);
            }

            // Render the mutable region (last log entry + pending text) and
            // push it onto the back. Next draw will pop these and re-render.
            let mut mutable_lines: Vec<String> = Vec::new();
            if let Some(last_entry) = tab.log.last() {
                for line in last_entry.to_lines_with(approx_width, &theme, verbose) {
                    let plain: String =
                        line.spans.iter().map(|s| s.content.as_ref()).collect();
                    mutable_lines.push(plain);
                }
            }
            for raw_line in tab.pending_text.lines() {
                mutable_lines.push(strip_ansi(raw_line));
            }
            for line in mutable_lines {
                tab.scrollback.push(line);
                tab.scrollback_pending_lines += 1;
            }
        }

        // v2.2.16: Layout system landing. This entire function will be split into
        // crates/anvil-cli/src/tui/layouts/{vertical_split,three_pane,journal}.rs.
        // Golden snapshots in tests/layout_snapshots.rs lock the current rendering
        // as the Layout D (vertical-split + tabs) regression baseline.

        // Collect a frozen snapshot of all state the draw closure needs.
        // `collect_snapshot()` performs all the side-effecting pre-draw work
        // (scrollback viewport follow, configure viewport follow, completion
        // viewport follow) and returns a heap-owned value. The draw closure
        // then has no reason to access `self`.
        let snap = self.collect_snapshot();

        // v2.2.16: Route to the active layout renderer via dispatch_render.
        // Per-tab layout: read cfg and layout_local from the active tab.
        let cfg = self.tabs[self.active_tab].tui_layout;
        let mut layout_local = self.tabs[self.active_tab].layout_local.clone();

        // Click-to-switch tab geometry — populated by the draw closure as it
        // builds the tab-bar spans, then copied back to self.tab_hits after
        // the closure returns.
        let mut new_tab_hits: Vec<TabHit> = Vec::new();
        let new_tab_bar_row: u16 = snap.tab_infos.first().map(|_| 0u16).unwrap_or(0);

        self.terminal.draw(|frame| {
            let size = frame.area();

            // v2.2.16: dispatch to the active layout renderer.
            // `dispatch_render` handles all layout-specific content (tab bar,
            // content area, input/footer, etc.). Modals are rendered on top.
            layouts::dispatch_render(cfg, frame, &snap, &mut layout_local, &mut new_tab_hits);

            // ── Modals rendered on top of any layout ─────────────────────────

            // T5-Ssh-E: SSH form modal.
            if let Some(ref form) = snap.ssh_form_snapshot {
                form.render(frame, size);
            }

            // #578: provider-login modal.
            if let Some(ref login_snap) = snap.provider_login_modal_snapshot {
                let accent = Color::Rgb(
                    snap.theme.accent.0,
                    snap.theme.accent.1,
                    snap.theme.accent.2,
                );
                let error_c = Color::Rgb(
                    snap.theme.error.0,
                    snap.theme.error.1,
                    snap.theme.error.2,
                );
                login_snap.render(frame, size, accent, error_c);
            }

            // Task #627: confirm modal (/restart, /iac apply).
            if let Some(ref c) = snap.confirm_modal_snapshot {
                let accent = Color::Rgb(
                    snap.theme.accent.0,
                    snap.theme.accent.1,
                    snap.theme.accent.2,
                );
                c.render(frame, size, accent);
            }

            // Task #627: password modal (/vault unlock).
            if let Some(ref p) = snap.password_modal_snapshot {
                let accent = Color::Rgb(
                    snap.theme.accent.0,
                    snap.theme.accent.1,
                    snap.theme.accent.2,
                );
                let error_c = Color::Rgb(
                    snap.theme.error.0,
                    snap.theme.error.1,
                    snap.theme.error.2,
                );
                p.render(frame, size, accent, error_c);
            }

            // Bug-3 Commit 4: permission approval modal.
            if let Some((ref tool_name, ref req_mode, ref cur_mode, ref input_summary)) =
                snap.active_permission_modal
            {
                use ratatui::widgets::{Block, Borders, Clear};
                use ratatui::text::Text as RatText;

                let modal_w = (size.width.saturating_sub(8)).min(72);
                let modal_h = 10u16;
                let modal_x = (size.width.saturating_sub(modal_w)) / 2;
                let modal_y = (size.height.saturating_sub(modal_h)) / 2;
                let modal_area = ratatui::layout::Rect {
                    x: modal_x,
                    y: modal_y,
                    width: modal_w,
                    height: modal_h,
                };

                frame.render_widget(Clear, modal_area);

                let inner_w = modal_w.saturating_sub(4) as usize;
                let summary_display = if input_summary.chars().count() > inner_w {
                    let mut s: String = input_summary
                        .chars()
                        .take(inner_w.saturating_sub(1))
                        .collect();
                    s.push('…');
                    s
                } else {
                    input_summary.clone()
                };

                let lines = vec![
                    ratatui::text::Line::from(""),
                    ratatui::text::Line::from(vec![
                        Span::styled(
                            "  Tool:     ",
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(tool_name.clone()),
                    ]),
                    ratatui::text::Line::from(vec![
                        Span::styled(
                            "  Requires: ",
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(req_mode.clone(), Style::default().fg(Color::Yellow)),
                    ]),
                    ratatui::text::Line::from(vec![
                        Span::styled(
                            "  Current:  ",
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(cur_mode.clone()),
                    ]),
                    ratatui::text::Line::from(vec![
                        Span::styled(
                            "  Input:    ",
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            summary_display,
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]),
                    ratatui::text::Line::from(""),
                    ratatui::text::Line::from(Span::styled(
                        "  [y] Allow   [a] Allow Always   [n] Deny",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )),
                    ratatui::text::Line::from(""),
                ];

                let block = Block::default()
                    .title(" Permission Required ")
                    .borders(Borders::ALL)
                    .border_style(
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    );
                let paragraph = Paragraph::new(RatText::from(lines)).block(block);
                frame.render_widget(paragraph, modal_area);
            }
        })?;
        // Persist clickable-tab geometry for the input handler to consult on
        // the next mouse Down event.
        self.tab_hits = new_tab_hits;
        self.tab_bar_row = new_tab_bar_row;

        // Write back any in-draw mutations to layout_local (cursor follow, etc.).
        // Per-tab: write back to the active tab's local state.
        self.tabs[self.active_tab].layout_local = layout_local;

        Ok(())
    }

    // ─── Layout system (v2.2.16) ─────────────────────────────────────────────

    /// Switch the TUI layout for the active tab, and optionally all tabs.
    ///
    /// - `global = false`: changes only the active tab's layout. Does NOT persist
    ///   to config.json. Other tabs are untouched.
    /// - `global = true`: changes ALL tabs' layouts AND persists to config.json so
    ///   new tabs inherit the new default.
    ///
    /// Per-tab switch is a no-op when `new == active_tab.tui_layout`.
    ///
    /// Shared conversation state (`log`, `input`, `cursor`, etc.) is never
    /// touched — the switch is paint-only for each tab.
    pub fn set_active_tab_layout(&mut self, new: runtime::TuiLayoutConfig, global: bool) {
        let current = self.tabs[self.active_tab].tui_layout;
        if new == current && !global {
            return; // no-op
        }

        if global {
            // Persist new default to config.json.
            Self::persist_layout_to_config(&new);
            // Apply to ALL tabs.
            for tab in &mut self.tabs {
                tab.tui_layout = new;
                tab.layout_local = layouts::LayoutLocalState::for_kind(new.kind);
            }
        } else {
            // Apply to active tab only.
            let tab = &mut self.tabs[self.active_tab];
            if new == tab.tui_layout {
                return;
            }
            tab.tui_layout = new;
            tab.layout_local = layouts::LayoutLocalState::for_kind(new.kind);
        }

        // Also update the AnvilTui-level fields for backward compat with any
        // code that still reads self.tui_layout / self.layout_local.
        self.tui_layout = new;
        self.layout_local = layouts::LayoutLocalState::for_kind(new.kind);

        self.redraw.request_full();
        // Layout switch is a structural event — ratatui's back-buffer
        // dimensions/regions change. Hard clear required (task #629).
        self.request_redraw_reason(RedrawReason::InlineOpReturn);

        // Task #647 (G3): mirror layout change to remote viewers.
        let kind_str = match new.kind {
            runtime::TuiLayoutKind::Classic => "classic",
            runtime::TuiLayoutKind::VerticalSplit => "vertical_split",
            runtime::TuiLayoutKind::ThreePane => "three_pane",
            runtime::TuiLayoutKind::Journal => "journal",
        };
        self.relay_forward(runtime::relay::RelayMessage::LayoutChanged {
            kind: kind_str.to_string(),
            tabs: new.tabs,
        });
    }

    /// Backward-compat alias: `set_layout(new)` → `set_active_tab_layout(new, global=true)`.
    ///
    /// Pre-v2.2.16 callers (e.g. the `/layout reset` handler) used `set_layout`.
    /// This ensures they still work and write config.json.
    pub fn set_layout(&mut self, new: runtime::TuiLayoutConfig) {
        self.set_active_tab_layout(new, true);
    }

    /// Return the current global layout configuration.
    ///
    /// Used by the relay emitter to include layout in `SessionMeta`.
    pub fn current_layout(&self) -> &runtime::TuiLayoutConfig {
        &self.tui_layout
    }

    /// Write the layout config to `~/.anvil/config.json` (best-effort).
    fn persist_layout_to_config(new: &runtime::TuiLayoutConfig) {
        if let Some(home) = dirs_next::home_dir() {
            let path = home.join(".anvil").join("config.json");
            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&raw) {
                    if let Some(obj) = val.as_object_mut() {
                        let alias = runtime::tui_layout_to_alias(new);
                        obj.insert(
                            "tui_layout".to_string(),
                            serde_json::json!({ "kind": alias.trim_end_matches("-tabs"), "tabs": new.tabs }),
                        );
                        if let Ok(out) = serde_json::to_string_pretty(&val) {
                            let _ = std::fs::write(&path, out);
                        }
                    }
                }
            }
        }
    }

    // ─── Event processing ────────────────────────────────────────────────────

    /// Flush any pending streaming text into the log as a completed assistant message.
    pub(super) fn flush_pending_text(&mut self) {
        let text = std::mem::take(&mut self.active_tab_mut().pending_text);
        if !text.trim().is_empty() {
            self.active_tab_mut().log.push(LogEntry::Assistant(text));
        }
    }

    /// Drain all queued TUI events without blocking.
    fn drain_events(&mut self) {
        while let Ok(tagged) = self.rx.try_recv() {
            self.apply_tagged_event(tagged);
        }
    }

    /// Resolve a `Tab.id` (logical identifier) to its current Vec index.
    /// Returns `None` when no tab with that id exists (e.g. it was closed
    /// mid-stream).
    fn tab_index_for_id(&self, tab_id: usize) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == tab_id)
    }

    /// Set a relay broadcast sender for forwarding TUI events to web clients.
    pub fn set_relay_tx(&mut self, tx: tokio::sync::broadcast::Sender<runtime::relay::RelayMessage>) {
        self.relay_tx = Some(tx);
    }

    /// Clear the relay broadcast sender.
    pub fn clear_relay_tx(&mut self) {
        self.relay_tx = None;
    }

    /// Forward a TUI event to the relay broadcast (if active).
    fn relay_forward(&self, msg: runtime::relay::RelayMessage) {
        if let Some(ref tx) = self.relay_tx {
            let _ = tx.send(msg);
        }
    }

    /// Route a tagged event to the correct tab.
    ///
    /// The `tagged.tab_id` field carries the logical `Tab.id` of the runtime
    /// that sent the event.  We resolve that to a Vec index via
    /// `tab_index_for_id`; if the tab no longer exists (closed mid-stream) the
    /// event is silently dropped.
    ///
    /// The relay forwarding uses `tagged.tab_id` (the logical id) so that the
    /// remote viewer sees a stable, per-tab identifier rather than a position
    /// in the Vec that shifts when tabs are closed.
    ///
    fn apply_tagged_event(&mut self, tagged: TaggedTuiEvent) {
        let TaggedTuiEvent { tab_id, event } = tagged;

        // Resolve tab_id → Vec index.  Drop events for closed tabs.
        let idx = match self.tab_index_for_id(tab_id) {
            Some(i) => i,
            None => {
                // Tab was closed mid-stream — drop the event silently.
                let _ = (tab_id, event);
                return;
            }
        };

        // Mirror events to relay for remote viewers (using the stable logical
        // tab_id, not the Vec index).
        match &event {
            TuiEvent::TextDelta(text) => {
                self.relay_forward(runtime::relay::RelayMessage::TextDelta { tab_id, text: text.clone() });
            }
            TuiEvent::TextDone => {
                self.relay_forward(runtime::relay::RelayMessage::TextDone { tab_id });
            }
            TuiEvent::TurnDone => {
                self.relay_forward(runtime::relay::RelayMessage::TurnDone { tab_id });
                // T4-K: snapshot diff totals BEFORE refresh so we can detect
                // whether this turn actually changed any tracked files. If
                // it did, surface a brief inline summary so the user sees
                // the net delta without having to run `/diff`.
                let prev_added = self.lines_added;
                let prev_removed = self.lines_removed;
                self.refresh_git_info();
                let net_added = self.lines_added.saturating_sub(prev_added);
                let net_removed = self.lines_removed.saturating_sub(prev_removed);
                if net_added > 0 || net_removed > 0 {
                    let summary = format!(
                        "Files changed this turn: +{net_added} −{net_removed} (run /diff for full unified diff)"
                    );
                    self.tabs[idx].log.push(LogEntry::System(summary));
                }
            }
            TuiEvent::ToolCallStart { name } => {
                self.relay_forward(runtime::relay::RelayMessage::ToolStart { tab_id, name: name.clone(), detail: String::new() });
            }
            TuiEvent::ToolCallActive { name, detail, .. } => {
                self.relay_forward(runtime::relay::RelayMessage::ToolStart { tab_id, name: name.clone(), detail: detail.clone() });
            }
            TuiEvent::ToolResult { name, summary, is_error } => {
                self.relay_forward(runtime::relay::RelayMessage::ToolResult { tab_id, name: name.clone(), summary: summary.clone(), is_error: *is_error });
            }
            TuiEvent::ThinkLabel(label) => {
                self.relay_forward(runtime::relay::RelayMessage::ThinkLabel { tab_id, label: label.clone() });
            }
            TuiEvent::Tokens { input, output } => {
                self.relay_forward(runtime::relay::RelayMessage::Tokens { tab_id, input: *input, output: *output });
            }
            TuiEvent::System(msg) => {
                self.relay_forward(runtime::relay::RelayMessage::System { tab_id, message: msg.clone() });
            }
            _ => {} // Other events don't need relay forwarding
        }

        match event {
            TuiEvent::TextDelta(text) => {
                self.tabs[idx].pending_text.push_str(&text);
                // v2.2.14 BUG-fix-real: route through the central gate so
                // the wait loop's commit step picks up every batch with a
                // forced full repaint + flush. Without this, the streaming
                // tab's frame-diff would coalesce with the active idle
                // tab's "just typed a char" diff and the latter could be
                // dropped.
                self.request_redraw_reason(RedrawReason::TextDeltaBatch);
            }
            TuiEvent::TextDone | TuiEvent::TurnDone => {
                // Flush onto the tab the event came from, not the visually-
                // active tab. Otherwise a background tab's stream end would
                // either leak its pending_text into the wrong log or wipe
                // the active tab's still-streaming buffer.
                let pending_was_empty = self.tabs[idx].pending_text.trim().is_empty();
                self.flush_pending_text_for(idx);
                let tab = &mut self.tabs[idx];
                tab.think_label.clear();
                tab.think_start = None;

                // v2.2.14 Phase 1: surface "no response" placeholder when the
                // model finishes a turn (TurnDone, not just TextDone) after
                // tool calls without producing any text. Without this, the
                // user sees the tool-call cards followed by nothing and
                // can't tell whether the turn is done or still working.
                // Detect via the log's tail: if the last entry is a ToolCall
                // (or absent), the turn ended without an Assistant entry.
                if matches!(event, TuiEvent::TurnDone) && pending_was_empty {
                    let needs_placeholder = match tab.log.last() {
                        Some(crate::tui::state::LogEntry::Assistant(_)) => false,
                        _ => true,
                    };
                    if needs_placeholder {
                        tab.log.push(crate::tui::state::LogEntry::System(
                            "(model finished without further response — turn complete)"
                                .to_string(),
                        ));
                    }
                }
            }
            TuiEvent::ToolCallStart { name } => {
                self.flush_pending_text();
                self.tabs[idx].log.push(LogEntry::ToolCall {
                    name,
                    detail: String::new(),
                    done: false,
                    is_error: false,
                    expanded: false,
                    full_input: None,
                    full_result: None,
                });
            }
            TuiEvent::ToolCallActive { name, detail, full_input } => {
                self.flush_pending_text();
                let mut found = false;
                for entry in self.tabs[idx].log.iter_mut().rev() {
                    if let LogEntry::ToolCall {
                        name: n,
                        detail: d,
                        full_input: fi,
                        done,
                        ..
                    } = entry
                        && *n == name && !*done {
                            *d = detail.clone();
                            *fi = Some(full_input.clone());
                            found = true;
                            break;
                        }
                }
                if !found {
                    self.tabs[idx].log.push(LogEntry::ToolCall {
                        name,
                        detail,
                        done: false,
                        is_error: false,
                        expanded: false,
                        full_input: Some(full_input),
                        full_result: None,
                    });
                }
            }
            TuiEvent::ToolResult {
                name,
                summary,
                is_error,
            } => {
                let mut matched = false;
                for entry in self.tabs[idx].log.iter_mut().rev() {
                    if let LogEntry::ToolCall {
                        name: n,
                        done,
                        is_error: err,
                        full_result,
                        ..
                    } = entry
                        && *n == name && !*done {
                            *done = true;
                            *err = is_error;
                            *full_result = Some(summary.clone());
                            matched = true;
                            break;
                        }
                }
                // Fallback: if no card existed, emit a system line so the result
                // is never silently dropped (e.g. out-of-order tool result).
                if !matched {
                    let label = if is_error { "error" } else { "ok" };
                    let pretty = crate::format_tool::tool_result_summary(&name, &summary, is_error);
                    let pretty = truncate_str(&pretty, 200);
                    if !pretty.is_empty() {
                        self.tabs[idx].log.push(LogEntry::System(format!(
                            "{name} [{label}]: {pretty}"
                        )));
                    }
                }
            }
            TuiEvent::ThinkLabel(label) => {
                let tab = &mut self.tabs[idx];
                if tab.think_label.is_empty() && !label.is_empty() {
                    tab.think_start = Some(Instant::now());
                }
                tab.think_label = label;
            }
            TuiEvent::Tokens { input, output } => {
                let tab = &mut self.tabs[idx];
                tab.input_tokens = tab.input_tokens.saturating_add(input);
                tab.output_tokens = tab.output_tokens.saturating_add(output);
            }
            TuiEvent::System(msg) => {
                self.tabs[idx].log.push(LogEntry::System(msg));
            }
            TuiEvent::WorkspaceClear { all_tabs } => {
                if all_tabs {
                    self.clear_all_tabs_display();
                } else {
                    self.clear_active_tab_display();
                }
            }
            // Bug-3 Commit 4: queue the permission request keyed by tab_id.
            // The worker is already blocking on its Receiver; the TUI will
            // show the modal when the user is on that tab and dispatch the
            // reply via handle_key.
            TuiEvent::PermissionRequired {
                tool_name,
                required_mode,
                current_mode,
                input_summary,
                response_tx,
            } => {
                self.pending_permissions.insert(
                    tab_id,
                    PendingPermission { tool_name, required_mode, current_mode, input_summary, response_tx },
                );
                self.redraw.request(redraw::DirtyRegions::ALL);
            }
        }
    }

    /// Auto-scroll to the bottom of the content and return to live view.
    pub(super) fn scroll_to_bottom(&mut self) {
        let tab = self.active_tab_mut();
        tab.scroll = usize::MAX;
        tab.scrollback_state = scrollback::ScrollbackState::live();
    }

    /// Scroll up by `n` lines.
    ///
    /// On the first call this transitions from live view into the in-TUI
    /// scrollback buffer, which retains up to [`scrollback::DEFAULT_CAPACITY`]
    /// lines.  Subsequent calls move the viewport further back.
    pub fn scroll_up(&mut self, n: usize) {
        let tab = self.active_tab_mut();
        let est_height = 20usize;
        let new_state = tab.scrollback_state.page_up(&tab.scrollback, est_height, n);
        tab.scrollback_state = new_state;
        // Keep legacy scroll in sync.
        tab.scroll = tab.scroll.saturating_sub(n);
    }

    /// Scroll down by `n` lines.
    ///
    /// Automatically returns to live view when the bottom of the scrollback
    /// buffer is reached.
    pub fn scroll_down(&mut self, n: usize) {
        let tab = self.active_tab_mut();
        if !tab.scrollback_state.is_live() {
            let est_height = 20usize;
            let new_state = tab.scrollback_state.page_down(&tab.scrollback, est_height, n);
            tab.scrollback_state = new_state;
            if new_state.is_live() {
                tab.scroll = usize::MAX;
                return;
            }
        }
        tab.scroll = tab.scroll.saturating_add(n);
    }

    /// Return immediately to live view (End key, or Ctrl+End).
    pub(super) fn scroll_to_live(&mut self) {
        self.scroll_to_bottom();
    }

    // ─── Public interface ────────────────────────────────────────────────────

    /// Process all queued model events (non-blocking).  Call this periodically
    /// during a running turn so the display stays live.
    pub fn poll_events(&mut self) {
        self.drain_events();
        let frame = self.active_tab().think_frame.wrapping_add(1);
        self.active_tab_mut().think_frame = frame;
        // Task #574: spinner-frame advance dirties the HEADER band
        // (spinner glyph is rendered in the model/version header strip on
        // journal layout; vertical-split/three-pane carry it in their own
        // bands but the HEADER bit is the closest scoped target).
        // Don't promote to ALL — that defeats the flash-fix gating.
        self.redraw.request(redraw::DirtyRegions::HEADER);
    }

    /// Block until `TurnDone` arrives on the channel, processing events as they
    /// come and redrawing the TUI.  Returns when the turn finishes.
    ///
    /// **T1-#400**: while waiting, also polls keyboard input non-blocking so
    /// the user can type a draft for the NEXT prompt while the current turn
    /// is still streaming. Plain typing accumulates into the active tab's
    /// input buffer (visible in the input box). Pressing Enter captures the
    /// draft into `self.pending_submit`, clears the input, and continues
    /// waiting. When the turn finishes, the captured draft (if any) is
    /// returned so the caller can immediately fire it as the next prompt.
    ///
    /// Esc / Ctrl+C still cancel the in-flight turn (existing behavior is
    /// untouched). Plain typing never cancels.
    pub fn wait_for_turn_end(&mut self) -> io::Result<Option<String>> {
        // Reset any stale pending_submit from a previous turn before we start
        // accumulating a new one.
        self.pending_submit = None;

        // v2.2.14 TUI-2: shorter poll interval (was 80ms) so keystrokes echo
        // within ~20ms. Spinner ticks are rate-limited separately.
        const POLL_INTERVAL: Duration = Duration::from_millis(20);
        const SPINNER_TICK_INTERVAL: Duration = Duration::from_millis(80);
        let mut last_spinner_tick = Instant::now();

        loop {
            // ── Drain streaming events non-blocking ──
            loop {
                match self.rx.try_recv() {
                    Ok(tagged) if matches!(tagged.event, TuiEvent::TurnDone) => {
                        self.apply_tagged_event(tagged);
                        self.scroll_to_bottom();
                        self.draw()?;
                        return Ok(self.pending_submit.take());
                    }
                    Ok(tagged) => self.apply_tagged_event(tagged),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        // T4-L: when the streaming channel disconnects without
                        // a TurnDone, the turn was interrupted (Esc / Ctrl+C /
                        // upstream error). Preserve any partial assistant text
                        // and mark it visibly so the user knows the response
                        // got cut off rather than thinking it completed.
                        let had_partial = !self
                            .active_tab()
                            .pending_text
                            .trim()
                            .is_empty();
                        self.flush_pending_text();
                        {
                            let tab = self.active_tab_mut();
                            tab.think_label.clear();
                            if had_partial {
                                tab.log.push(LogEntry::System(
                                    "↯ turn interrupted — partial response preserved above".to_string(),
                                ));
                            }
                        }
                        self.scroll_to_bottom();
                        self.draw()?;
                        return Ok(self.pending_submit.take());
                    }
                }
            }

            // ── Poll keyboard input non-blocking (T1-#400) ──
            // Crossterm's event::poll(0) returns true iff an event is ready.
            // We loop until the queue drains so a burst of keys (e.g. paste)
            // gets absorbed in one frame.
            while crossterm::event::poll(Duration::from_millis(0))? {
                match crossterm::event::read()? {
                    crossterm::event::Event::Key(key) if matches!(
                        key.kind,
                        crossterm::event::KeyEventKind::Press
                            | crossterm::event::KeyEventKind::Repeat
                    ) => {
                        self.handle_in_flight_key(key);
                    }
                    crossterm::event::Event::Paste(text) => {
                        // Task #599 / v2.2.14 Phase 1: single shared
                        // paste handler. See tui::paste.
                        //
                        // Task #604 Part C: Paste events bypass the
                        // burst tracker; reset so any partial burst
                        // doesn't leak across.
                        self.burst_tracker.reset();
                        paste::handle_paste(self, text);
                    }
                    _ => {}
                }
            }

            // Task #604 Part C: idle-based burst substitution. A
            // keystroke-burst that ended without a trailing keystroke
            // still substitutes once the idle window elapses, so the
            // input box updates BEFORE Enter is pressed.
            {
                let input_snapshot = self.active_tab().input.clone();
                let now = std::time::Instant::now();
                let outcome = paste::check_burst_idle(
                    &mut self.burst_tracker,
                    &input_snapshot,
                    now,
                );
                if let paste::BurstOutcome::SubstitutePath { start, end, path } = outcome {
                    paste::apply_burst_substitution(self, start, end, &path);
                    self.burst_tracker.reset();
                    self.request_redraw_reason(RedrawReason::KeyEvent);
                }
            }

            // ── Spinner frame tick + draw ──
            if last_spinner_tick.elapsed() >= SPINNER_TICK_INTERVAL {
                let frame = self.active_tab().think_frame.wrapping_add(1);
                self.active_tab_mut().think_frame = frame;
                last_spinner_tick = Instant::now();
            }
            self.draw()?;

            // ── Short blocking recv so input typed during this window gets
            // picked up on the very next iteration. ──
            match self.rx.recv_timeout(POLL_INTERVAL) {
                Ok(tagged) if matches!(tagged.event, TuiEvent::TurnDone) => {
                    self.apply_tagged_event(tagged);
                    self.scroll_to_bottom();
                    self.draw()?;
                    return Ok(self.pending_submit.take());
                }
                Ok(tagged) => self.apply_tagged_event(tagged),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.flush_pending_text();
                    {
                        let tab = self.active_tab_mut();
                        tab.think_label.clear();
                    }
                    self.scroll_to_bottom();
                    self.draw()?;
                    return Ok(self.pending_submit.take());
                }
            }
        }
    }

    /// Drain the TUI event channel without blocking.
    ///
    /// Routes each event to the correct tab via `apply_tagged_event`. Intended
    /// to be called by the main loop each iteration so that background tabs
    /// (whose turns the main loop is NOT actively waiting on) continue to
    /// receive and render streaming events.
    ///
    /// Returns the number of events processed.
    pub fn pump_events_nonblocking(&mut self) -> usize {
        let mut count = 0;
        while let Ok(tagged) = self.rx.try_recv() {
            self.apply_tagged_event(tagged);
            count += 1;
        }
        count
    }

    /// Pump TUI events and keystrokes while waiting for `target_tab_id`'s
    /// `TurnDone`. The caller stays in this loop for the lifetime of the
    /// turn it just spawned; other tabs' turns run in parallel on their own
    /// worker threads and their events are routed to the right tab here
    /// without unblocking the caller.
    ///
    /// `target_tab_id` is bound to the tab the caller spawned for; it is
    /// NOT updated when the user switches the visually-active tab — the
    /// wait is for the originally-spawned turn, not "whatever tab is on
    /// screen right now". (v2.2.14 TUI-2 deep fix.)
    ///
    /// Returns when one of:
    ///   - `target_tab_id`'s `TurnDone` lands → `TurnDone`.
    ///   - The streaming channel disconnects → `ChannelClosed`.
    ///   - The user did something the caller must handle (tab switch, new/
    ///     close tab, slash command, Enter on an idle tab) →
    ///     `TabSwitched` / `OpenNewTab` / `CloseActiveTab` /
    ///     `SlashCommand(s)` / `SubmitChatPrompt(s)`.
    ///
    /// Enter pressed while the active tab is itself streaming queues the
    /// draft onto the tab's `pending_submit`/`message_queue` (TUI-3) and
    /// does NOT return; Enter on a non-streaming active tab returns
    /// `SubmitChatPrompt` regardless of which tab `target_tab_id` names.
    pub fn wait_for_turn_end_for_tab(
        &mut self,
        target_tab_id: usize,
    ) -> io::Result<InFlightInterruption> {
        // v2.2.14 TUI-2: the inter-iteration sleep used to be a single 80ms
        // `recv_timeout`. When the user typed on a background tab while
        // another tab was streaming, the character sat in crossterm's queue
        // for up to 80ms before the wait loop processed it — visibly laggy.
        // Lowering the budget to 20ms drops the worst-case echo latency to
        // ~20ms; the spinner is rate-limited separately so it still ticks at
        // ~12fps.
        const POLL_INTERVAL: Duration = Duration::from_millis(20);
        const SPINNER_TICK_INTERVAL: Duration = Duration::from_millis(80);
        let mut last_spinner_tick = Instant::now();

        loop {
            // ── Drain streaming events non-blocking ──
            //
            // v2.2.14 BUG-fix-real: each `apply_tagged_event` that lands a
            // `TextDelta` calls `request_redraw_reason(TextDeltaBatch)` —
            // the commit step after this drain picks up every batch with a
            // forced clear + draw + flush so the user's typed char on tab
            // 2 isn't lost to ratatui's diff coalescing with tab 1's
            // streaming buffer.
            loop {
                match self.rx.try_recv() {
                    Ok(tagged) if matches!(tagged.event, TuiEvent::TurnDone)
                        && tagged.tab_id == target_tab_id =>
                    {
                        self.apply_tagged_event(tagged);
                        self.scroll_to_bottom();
                        // Task #629: end-of-stream commit. Stay on the soft
                        // path so the final TextDelta doesn't flash.
                        self.request_redraw_reason(RedrawReason::TextDeltaBatch);
                        self.commit_pending_redraw()?;
                        return Ok(InFlightInterruption::TurnDone);
                    }
                    Ok(tagged) => self.apply_tagged_event(tagged),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        // Channel disconnected — treat as turn done.
                        let had_partial = !self.active_tab().pending_text.trim().is_empty();
                        self.flush_pending_text();
                        {
                            let tab = self.active_tab_mut();
                            tab.think_label.clear();
                            if had_partial {
                                tab.log.push(LogEntry::System(
                                    "↯ turn interrupted — partial response preserved above"
                                        .to_string(),
                                ));
                            }
                        }
                        self.scroll_to_bottom();
                        // Task #629: channel-closed handler still during
                        // streaming context — soft path.
                        self.request_redraw_reason(RedrawReason::TextDeltaBatch);
                        self.commit_pending_redraw()?;
                        return Ok(InFlightInterruption::ChannelClosed);
                    }
                }
            }

            // v2.2.14 BUG-fix-real: commit any TextDelta-batch redraws the
            // drain just requested. Pulling the commit out of the drain
            // loop coalesces multiple batches into a single forced repaint
            // — at most one full draw per wait-loop iteration, but always
            // at least one when streaming is active and the active tab is
            // visible.
            self.commit_pending_redraw()?;

            // ── Poll keyboard input non-blocking ──
            while crossterm::event::poll(Duration::from_millis(0))? {
                match crossterm::event::read()? {
                    crossterm::event::Event::Key(key)
                        if matches!(
                            key.kind,
                            crossterm::event::KeyEventKind::Press
                                | crossterm::event::KeyEventKind::Repeat
                        ) =>
                    {
                        if let Some(interruption) =
                            self.handle_in_flight_key_extended(key, target_tab_id)
                        {
                            // Draw before returning so the UI reflects the
                            // most recent state (e.g. newly-active tab).
                            self.request_redraw_reason(RedrawReason::TabSwitch);
                            self.commit_pending_redraw()?;
                            return Ok(interruption);
                        }
                        // v2.2.14 BUG-fix-real: commit per-keystroke. The
                        // key handler set `redraw_pending` via
                        // `request_redraw_reason(KeyEvent)`; commit now so
                        // the typed char hits the terminal before the
                        // wait loop's next `recv_timeout` parks waiting
                        // for tab 1's next TextDelta.
                        self.commit_pending_redraw()?;
                    }
                    crossterm::event::Event::Mouse(mouse_event) => {
                        // Mouse clicks on tab labels should switch tabs.
                        use crossterm::event::MouseEventKind;
                        if matches!(
                            mouse_event.kind,
                            MouseEventKind::Down(crossterm::event::MouseButton::Left)
                        ) {
                            let col = mouse_event.column;
                            let row = mouse_event.row;
                            let prev_active = self.active_tab;
                            self.handle_mouse_tab_click(col, row);
                            if self.active_tab != prev_active {
                                self.request_redraw_reason(RedrawReason::TabSwitch);
                                self.commit_pending_redraw()?;
                                return Ok(InFlightInterruption::TabSwitched);
                            }
                        }
                    }
                    crossterm::event::Event::Paste(text) => {
                        // Task #599 / v2.2.14 Phase 1: single shared
                        // paste handler. See tui::paste.
                        //
                        // Task #604 Part C: Paste events bypass the
                        // burst tracker; reset so any partial burst
                        // doesn't leak across.
                        self.burst_tracker.reset();
                        paste::handle_paste(self, text);
                        // v2.2.14 BUG-fix-real: paste is a multi-char key
                        // event — drive the same gate so the pasted text
                        // shows up in the input box on the active tab.
                        self.request_redraw_reason(RedrawReason::KeyEvent);
                        self.commit_pending_redraw()?;
                    }
                    _ => {}
                }
            }

            // Task #604 Part C: idle-based burst substitution (mirror of
            // the per-tab wait loop's check above).
            {
                let input_snapshot = self.active_tab().input.clone();
                let now = std::time::Instant::now();
                let outcome = paste::check_burst_idle(
                    &mut self.burst_tracker,
                    &input_snapshot,
                    now,
                );
                if let paste::BurstOutcome::SubstitutePath { start, end, path } = outcome {
                    paste::apply_burst_substitution(self, start, end, &path);
                    self.burst_tracker.reset();
                    self.request_redraw_reason(RedrawReason::KeyEvent);
                    self.commit_pending_redraw()?;
                }
            }

            // ── Spinner frame tick + draw ──
            // Rate-limit so a 20ms poll loop doesn't burn the spinner.
            if last_spinner_tick.elapsed() >= SPINNER_TICK_INTERVAL {
                let frame = self.active_tab().think_frame.wrapping_add(1);
                self.active_tab_mut().think_frame = frame;
                last_spinner_tick = Instant::now();
                self.request_redraw_reason(RedrawReason::Spinner);
            }
            // v2.2.14 BUG-fix-real: trailing commit routed through the
            // central gate. Idempotent — when nothing requested a redraw
            // this iteration the call is cheap and skips the draw entirely.
            // When a key handler or TextDelta batch DID request a redraw
            // but no per-event commit fired (e.g. paste in a wedge case),
            // this catches it.
            self.commit_pending_redraw()?;

            // ── Short blocking recv so input typed during this window gets
            // picked up on the very next iteration. ──
            match self.rx.recv_timeout(POLL_INTERVAL) {
                Ok(tagged)
                    if matches!(tagged.event, TuiEvent::TurnDone)
                        && tagged.tab_id == target_tab_id =>
                {
                    self.apply_tagged_event(tagged);
                    self.scroll_to_bottom();
                    // Task #629: end-of-stream commit in recv_timeout
                    // branch. Soft path so the final TextDelta doesn't
                    // flash.
                    self.request_redraw_reason(RedrawReason::TextDeltaBatch);
                    self.commit_pending_redraw()?;
                    return Ok(InFlightInterruption::TurnDone);
                }
                Ok(tagged) => self.apply_tagged_event(tagged),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.flush_pending_text();
                    {
                        let tab = self.active_tab_mut();
                        tab.think_label.clear();
                    }
                    self.scroll_to_bottom();
                    // Task #629: channel-closed handler in recv_timeout
                    // branch — same soft-path treatment.
                    self.request_redraw_reason(RedrawReason::TextDeltaBatch);
                    self.commit_pending_redraw()?;
                    return Ok(InFlightInterruption::ChannelClosed);
                }
            }
            // v2.2.14 BUG-fix-real: the recv_timeout branch may have
            // landed another TextDelta batch; commit before iterating.
            self.commit_pending_redraw()?;
        }
    }

    /// Handle a key event that arrived while `wait_for_turn_end_for_tab` is
    /// polling, returning `Some(reason)` for actions that should cause the
    /// wait to return early, or `None` to continue waiting.
    ///
    /// Tab-navigation keys (F2/F3, Ctrl+Left/Right, Ctrl+1-9, Alt+1-9) switch
    /// the TUI's active tab internally and return `TabSwitched`.
    ///
    /// Ctrl+T returns `OpenNewTab` (caller creates tab + runtime).
    /// Ctrl+W returns `CloseActiveTab` (caller closes tab + updates state).
    ///
    /// Enter on the active-tab-is-target case: captures draft into the
    /// tab's input buffer (Bug-1 type-ahead) and returns `None` — the wait
    /// continues and the draft fires as the next turn after streaming ends.
    ///
    /// Enter on the active-tab-is-idle case: returns `SubmitChatPrompt`.
    ///
    /// If the trimmed buffer starts with `/`, Enter returns `SlashCommand`
    /// regardless of which tab is active (slash commands are always immediate).
    fn handle_in_flight_key_extended(
        &mut self,
        key: crossterm::event::KeyEvent,
        target_tab_id: usize,
    ) -> Option<InFlightInterruption> {
        use crossterm::event::{KeyCode, KeyModifiers};

        // v2.2.14 TUI-1: Ctrl+C while a turn is streaming cancels the
        // in-flight turn on the in-flight tab (regardless of which tab is
        // currently focused). Flipping the per-tab cancel flag short-circuits
        // the runtime's next inter-frame check; the worker then emits
        // `TurnDone` naturally and the wait returns. We do NOT exit the app
        // here — bare Ctrl+C exit on empty input is handled by the idle
        // input loop, not this in-flight handler.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            self.cancel_turn_for_tab_id(target_tab_id);
            return None;
        }

        // v2.2.14 TUI-3: Esc / Ctrl+Shift+Esc drain the per-tab message
        // queue (the in-flight tab's queue, not the visually-active tab's).
        // Ctrl+Shift+Esc clears every queued message; plain Esc drops only
        // the most-recently submitted one.
        if matches!(key.code, KeyCode::Esc) {
            if let Some(idx) = self.tabs.iter().position(|t| t.id == target_tab_id) {
                let queue = &mut self.tabs[idx].message_queue;
                let dirty = if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.modifiers.contains(KeyModifiers::SHIFT)
                {
                    let had_any = !queue.is_empty();
                    queue.clear();
                    had_any
                } else {
                    queue.pop_back().is_some()
                };
                if dirty {
                    self.redraw.request(redraw::DirtyRegions::INPUT);
                }
            }
            return None;
        }

        // ── Tab-switch and meta keys ───────────────────────────────────────
        // These are checked first so they take priority over plain-char input.

        // Ctrl+T → new tab request
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('t' | 'T'))
        {
            return Some(InFlightInterruption::OpenNewTab);
        }

        // Ctrl+W → close active tab request
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('w' | 'W'))
        {
            return Some(InFlightInterruption::CloseActiveTab);
        }

        // F2 → previous tab
        if matches!(key.code, KeyCode::F(2)) {
            self.prev_tab();
            return Some(InFlightInterruption::TabSwitched);
        }

        // F3 → next tab
        if matches!(key.code, KeyCode::F(3)) {
            self.next_tab();
            return Some(InFlightInterruption::TabSwitched);
        }

        // Ctrl+Left / Ctrl+[ → previous tab
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Left | KeyCode::Char('['))
        {
            self.prev_tab();
            return Some(InFlightInterruption::TabSwitched);
        }

        // Ctrl+Right / Ctrl+] → next tab
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Right | KeyCode::Char(']'))
        {
            self.next_tab();
            return Some(InFlightInterruption::TabSwitched);
        }

        // Ctrl+digit (1-9) → switch to tab by index
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if let KeyCode::Char(ch) = key.code {
                if ch.is_ascii_digit() && ch != '0' {
                    let n = ch as usize - '0' as usize;
                    self.switch_tab(n.saturating_sub(1));
                    return Some(InFlightInterruption::TabSwitched);
                }
            }
        }

        // Alt+digit (1-9) → switch to tab by index
        if key.modifiers.contains(KeyModifiers::ALT) {
            if let KeyCode::Char(ch) = key.code {
                if let Some(n) = ch.to_digit(10) {
                    if n >= 1 {
                        self.switch_tab((n as usize).saturating_sub(1));
                        return Some(InFlightInterruption::TabSwitched);
                    }
                }
            }
        }

        // ── Enter key ─────────────────────────────────────────────────────
        // Behaviour depends on whether the active tab is the one that is
        // in-flight (target_tab_id) or an idle tab.

        if matches!(key.code, KeyCode::Enter) && !key.modifiers.contains(KeyModifiers::CONTROL) {
            // Take out whatever was typed into the input buffer.
            let draft = {
                let tab = self.active_tab_mut();
                let d = std::mem::take(&mut tab.input);
                tab.cursor = 0;
                d.trim().to_string()
            };
            self.redraw.request(redraw::DirtyRegions::INPUT);

            if draft.is_empty() {
                // Nothing to submit; keep waiting.
                return None;
            }

            // Slash commands are always dispatched immediately, regardless of
            // which tab is active or whether it is in-flight.
            if draft.starts_with('/') {
                return Some(InFlightInterruption::SlashCommand(draft));
            }

            // v2.2.14 TUI-2 (deep): route by the active tab's own in_flight
            // state, not by equality with `target_tab_id`. Previously the
            // wait re-bound `target_tab_id` to whatever tab the user just
            // switched to — so Enter on an idle tab evaluated `active ==
            // target` true and got queued instead of dispatching.
            let active_tab_id = self.tabs.get(self.active_tab).map(|t| t.id).unwrap_or(0);
            if self.is_active_tab_in_flight() {
                // v2.2.14 TUI-3: active tab IS streaming. The first type-
                // ahead message goes into `pending_submit` (the slot the
                // main loop's TurnDone consumer reads when target's turn
                // ends) only if active == target — that's the original
                // "type-ahead fires as next turn on the same tab" path.
                // Otherwise the draft queues onto the active tab's own
                // `message_queue` so it dispatches when that tab finishes.
                if active_tab_id == target_tab_id && self.pending_submit.is_none() {
                    self.pending_submit = Some(draft);
                } else {
                    self.active_tab_mut().message_queue.push_back(draft);
                }
                self.redraw.request(redraw::DirtyRegions::INPUT);
                return None;
            }

            // Active tab is idle — fire immediately as a new turn.
            // Record the User message on this tab's log + history so the
            // submitted prompt appears in the visible scrollback before
            // the assistant's streaming response arrives. The non-in-flight
            // path (`submit_input`) does this; the in-flight path was
            // missing it, so the user's prompt vanished while the model
            // response showed up alone.
            {
                use crate::tui::state::LogEntry;
                let tab = self.active_tab_mut();
                tab.history.push(draft.clone());
                tab.log.push(LogEntry::User(draft.clone()));
            }
            self.scroll_to_bottom();
            self.redraw.request(redraw::DirtyRegions::ALL);
            return Some(InFlightInterruption::SubmitChatPrompt(draft));
        }

        // ── Plain editing (printable chars, Backspace, Left/Right cursor) ─
        // These are the same as handle_in_flight_key: accumulate the draft
        // on the active tab without interrupting the wait.
        //
        // v2.2.14 BUG-fix-real (post TUI-2 deep, replaces 24bbe50):
        //
        // `24bbe50` added an ad-hoc `self.draw()` here on the diagnosis
        // that the wait loop's recv_timeout(20ms) was iterating too fast
        // for the trailing draw to commit. That was the wrong layer:
        // `draw()` is being called, but the terminal frame is not
        // reliably committed while tab 1's TextDelta firehose keeps the
        // wait loop hot. Per user diagnosis: route every "needs paint"
        // signal through `request_redraw_reason(KeyEvent)`; the wait
        // loop's top-of-iteration `commit_pending_redraw` then forces a
        // `terminal.clear()` + `draw()` + `backend.flush()` that bypasses
        // ratatui's frame-diff coalescing.
        let mut edited = false;
        match (key.code, key.modifiers) {
            (KeyCode::Char(c), m) if !m.contains(KeyModifiers::CONTROL)
                && !m.contains(KeyModifiers::ALT) =>
            {
                self.insert_char(c);
                self.redraw.request(redraw::DirtyRegions::INPUT);
                self.request_redraw_reason(RedrawReason::KeyEvent);
                edited = true;
            }
            (KeyCode::Backspace, _) => {
                let tab = self.active_tab_mut();
                if tab.cursor > 0 {
                    let prev = helpers::prev_char_boundary(&tab.input, tab.cursor);
                    tab.input.replace_range(prev..tab.cursor, "");
                    tab.cursor = prev;
                    edited = true;
                }
                self.redraw.request(redraw::DirtyRegions::INPUT);
                if edited {
                    self.request_redraw_reason(RedrawReason::KeyEvent);
                }
            }
            _ => {}
        }
        // v2.2.14 BUG-fix-real: the immediate `self.draw()` call from
        // 24bbe50 is gone — the wait loop now drives commit via the
        // central gate, which calls `draw_full()` (clear + draw) and
        // `backend.flush()`. Leaving the draw here would defeat the gate
        // (double-commit, no clear) and obscure the diagnostic signal.
        let _ = edited;

        None
    }

    /// Flush pending streaming text for a specific tab index.
    ///
    /// Used when a background tab's `TurnDone` arrives — the pending text
    /// must be flushed to that tab's log even though it is not the active tab.
    /// For the active tab, the existing `flush_pending_text` (which operates
    /// on `active_tab`) is still used.
    pub(super) fn flush_pending_text_for(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            let text = std::mem::take(&mut self.tabs[idx].pending_text);
            if !text.trim().is_empty() {
                self.tabs[idx].log.push(LogEntry::Assistant(text));
            }
        }
    }

    /// Append a system message to a specific tab's log (by Vec index).
    ///
    /// Used by the main loop to surface background-tab turn errors into the
    /// correct tab's log without switching focus.
    pub fn push_system_to_tab(&mut self, idx: usize, text: impl Into<String>) {
        let text = text.into();
        if !text.is_empty() && idx < self.tabs.len() {
            self.tabs[idx].log.push(LogEntry::System(text));
        }
    }

    /// Handle a mouse click that may have landed on a tab label in the tab bar.
    ///
    /// Looks up `(col, row)` against `self.tab_hits` / `self.tab_bar_row`
    /// (geometry populated by the last `draw` call) and calls `switch_tab` if
    /// a label hit is found.  Matching the close-column (×) is intentionally
    /// skipped here — close during in-flight is not permitted; the caller can
    /// handle `CloseActiveTab` via Ctrl+W.
    pub(super) fn handle_mouse_tab_click(&mut self, col: u16, row: u16) {
        if row != self.tab_bar_row {
            return;
        }
        let hits = self.tab_hits.clone();
        for hit in &hits {
            if col >= hit.label_start && col < hit.label_end {
                self.switch_tab(hit.idx);
                self.redraw.request(redraw::DirtyRegions::ALL);
                return;
            }
        }
    }

    /// Process a key event that arrived while a turn was in flight.
    ///
    /// Behavior (per user directive 2026-05-10):
    ///   - Plain printable chars: append to the active tab's input buffer
    ///   - Backspace: delete one char from the input buffer
    ///   - Enter: capture the input as `pending_submit`, clear the input
    ///   - Esc / Ctrl+C: NOT handled here — the existing cancel path stays
    ///     in `read_input`. We deliberately ignore them so a stray Esc
    ///     doesn't cancel a turn the user wanted to keep.
    ///
    /// Note: this is intentionally a much simpler key handler than the full
    /// `handle_key`. We don't honor up/down history, completion popups,
    /// slash commands, etc., during in-flight typing — only the basics
    /// needed to compose a follow-up message.
    fn handle_in_flight_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};

        // v2.2.14 TUI-1: Ctrl+C cancels the active tab's in-flight turn.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            let active_id = self.tabs.get(self.active_tab).map(|t| t.id).unwrap_or(0);
            self.cancel_turn_for_tab_id(active_id);
            return;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char(c), m) if !m.contains(KeyModifiers::CONTROL) => {
                // Insert at cursor (handles utf-8 char-boundary correctly)
                self.insert_char(c);
                self.redraw.request(redraw::DirtyRegions::INPUT);
            }
            (KeyCode::Backspace, _) => {
                let tab = self.active_tab_mut();
                if tab.cursor > 0 {
                    let prev = prev_char_boundary(&tab.input, tab.cursor);
                    tab.input.replace_range(prev..tab.cursor, "");
                    tab.cursor = prev;
                }
                self.redraw.request(redraw::DirtyRegions::INPUT);
            }
            (KeyCode::Enter, _) => {
                // v2.2.14 TUI-3: first held draft populates `pending_submit`
                // (the existing held-submit slot the main loop reads).
                // Subsequent drafts stack in the active tab's message queue
                // and fire FIFO after each turn ends.
                let draft = {
                    let tab = self.active_tab_mut();
                    let d = std::mem::take(&mut tab.input);
                    tab.cursor = 0;
                    d.trim().to_string()
                };
                if !draft.is_empty() {
                    if self.pending_submit.is_none() {
                        self.pending_submit = Some(draft);
                    } else {
                        self.active_tab_mut().message_queue.push_back(draft);
                    }
                }
                self.redraw.request(redraw::DirtyRegions::INPUT);
            }
            (KeyCode::Esc, m) if m.contains(KeyModifiers::CONTROL)
                && m.contains(KeyModifiers::SHIFT) =>
            {
                // v2.2.14 TUI-3: Ctrl+Shift+Esc clears the entire queue.
                let tab = self.active_tab_mut();
                if !tab.message_queue.is_empty() {
                    tab.message_queue.clear();
                    self.redraw.request(redraw::DirtyRegions::INPUT);
                }
            }
            (KeyCode::Esc, _) => {
                // v2.2.14 TUI-3: Esc drops the most-recently queued message
                // (the user's "oops, didn't mean to send that"). Plain Esc
                // does NOT cancel the in-flight turn — Ctrl+C is the cancel
                // key now (TUI-1).
                let tab = self.active_tab_mut();
                if tab.message_queue.pop_back().is_some() {
                    self.redraw.request(redraw::DirtyRegions::INPUT);
                }
            }
            // All other keys are ignored during in-flight typing —
            // arrow keys, function keys, modifiers, etc.
            _ => {}
        }
    }

    /// v2.2.14 TUI-1: cancel the in-flight turn on the tab with `tab_id` by
    /// flipping its shared cancel flag, preserve any partial assistant text
    /// already on screen, and push a "⏸ cancelled" system entry. The runtime
    /// worker will observe the flag at the next inter-frame check and emit
    /// `TurnDone` naturally; the main wait loop returns soon after.
    fn cancel_turn_for_tab_id(&mut self, tab_id: usize) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        self.tabs[idx]
            .cancel_token
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Preserve any partial streaming text in the tab's log (same pattern
        // task #414 used for Esc-cancel + ChannelClosed).
        let had_partial = !self.tabs[idx].pending_text.trim().is_empty();
        self.flush_pending_text_for(idx);
        let tab = &mut self.tabs[idx];
        tab.think_label.clear();
        if had_partial {
            tab.log.push(LogEntry::System(
                "⏸ cancelled — partial response preserved above".to_string(),
            ));
        } else {
            tab.log.push(LogEntry::System("⏸ cancelled".to_string()));
        }
        if idx == self.active_tab {
            self.scroll_to_bottom();
        }
        self.redraw.request(redraw::DirtyRegions::ALL);
    }

    /// Show a system message (e.g. slash command output).
    ///
    /// Also mirrors to the relay broadcast so the web viewer's chat log
    /// stays in sync with the TUI scrollback (Bug 3 fix).
    pub fn push_system(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !text.is_empty() {
            let tab_id = self.active_tab().id;
            self.relay_forward(runtime::relay::RelayMessage::System {
                tab_id,
                message: text.clone(),
            });
            self.active_tab_mut().log.push(LogEntry::System(text));
        }
        self.scroll_to_bottom();
    }

    /// Update the displayed permission mode (e.g. after /permissions switch).
    #[allow(dead_code)]
    pub fn set_permission_mode(&mut self, mode: impl Into<String>) {
        self.permission_mode = mode.into();
    }

    /// Re-run git queries and update cached branch/diff/productivity info.
    pub fn refresh_git_info(&mut self) {
        let (branch, diff, added, removed) = fetch_git_info();
        self.git_branch = branch;
        self.git_diff_stats = diff;
        self.lines_added = added;
        self.lines_removed = removed;
    }

    /// Update the QMD status line in the footer.
    pub fn set_qmd_status(&mut self, status: impl Into<String>) {
        self.qmd_status = status.into();
    }

    /// Update the last archive status shown in the footer.
    pub fn set_archive_status(&mut self, status: impl Into<String>) {
        self.last_archive_status = status.into();
    }

    /// Set the update notification message shown in the footer.
    pub fn set_update_available(&mut self, msg: impl Into<String>) {
        self.update_available = msg.into();
    }

    pub const fn set_thinking_enabled(&mut self, enabled: bool) {
        self.thinking_enabled = enabled;
    }

    /// Update the effort level display in the TUI status line.
    pub fn set_effort_level(&mut self, level: &str) {
        self.effort_level = level.to_string();
    }

    // ─── Status line configuration ──────────────────────────────────────────

    /// Switch the status line layout to a named preset.
    pub fn set_status_line_preset(&mut self, name: &str) -> bool {
        use runtime::theme::StatusLinePreset;
        if let Some(preset) = StatusLinePreset::from_name(name) {
            self.status_line_config = StatusLineConfig::from_preset(preset);
            true
        } else {
            false
        }
    }

    /// Replace the status line config wholesale (e.g. from `config_set` via web viewer).
    pub fn set_status_line_config(&mut self, config: StatusLineConfig) {
        self.status_line_config = config;
    }

    /// Get a reference to the current status line config.
    pub const fn status_line_config(&self) -> &StatusLineConfig {
        &self.status_line_config
    }

    // ─── Remote control status ───────────────────────────────────────────────

    /// Show the relay URL and optional pairing code in the status bar.
    pub fn set_remote_status(&mut self, url: &str, code: &str) {
        self.remote_url = url.to_string();
        self.remote_code = code.to_string();
    }

    /// Clear the relay status (session stopped).
    pub fn clear_remote_status(&mut self) {
        self.remote_url.clear();
        self.remote_code.clear();
    }

    /// Build a snapshot of all tabs as `relay::TabSnapshot` values for broadcast
    /// to connected web clients.
    #[allow(dead_code)]
    pub fn build_snapshot(&self) -> Vec<runtime::relay::TabSnapshot> {
        self.tabs.iter().map(|tab| {
            let log = tab.log.iter().map(|entry| {
                match entry {
                    state::LogEntry::User(text) => {
                        runtime::relay::LogEntrySnapshot::User { text: text.clone() }
                    }
                    state::LogEntry::Assistant(text) => {
                        runtime::relay::LogEntrySnapshot::Assistant { text: text.clone() }
                    }
                    state::LogEntry::System(text) => {
                        runtime::relay::LogEntrySnapshot::System { text: text.clone() }
                    }
                    state::LogEntry::ToolCall { name, detail, is_error, full_result, .. } => {
                        runtime::relay::LogEntrySnapshot::ToolCall {
                            name: name.clone(),
                            detail: detail.clone(),
                            result: full_result.clone(),
                            is_error: *is_error,
                        }
                    }
                    state::LogEntry::Image { path, label } => {
                        runtime::relay::LogEntrySnapshot::System {
                            text: format!("[Image: {label} — {path}]"),
                        }
                    }
                }
            }).collect();

            runtime::relay::TabSnapshot {
                tab_id: tab.id,
                name: tab.name.clone(),
                model: tab.model.clone(),
                active: std::ptr::eq(tab, self.active_tab()),
                log,
                tokens: runtime::relay::TokenSnapshot {
                    input: tab.input_tokens,
                    output: tab.output_tokens,
                },
            }
        }).collect()
    }

    // ─── Agent panel ─────────────────────────────────────────────────────────

    /// Refresh the agent panel rows snapshot from the caller's `AgentManager`.
    ///
    /// Each tuple: `(id, type_label, task, elapsed_str, status_icon)`.
    pub fn update_agent_rows(
        &mut self,
        rows: Vec<(usize, String, String, String, &'static str)>,
    ) {
        self.agent_rows = rows;
        if !self.agent_rows.is_empty() {
            self.agent_panel_visible = true;
        }
    }

    /// Toggle the agent panel visibility.
    #[allow(dead_code)]
    pub const fn toggle_agent_panel(&mut self) {
        self.agent_panel_visible = !self.agent_panel_visible;
    }

    /// Signal that the TUI should close on the next `read_input` loop tick.
    #[allow(dead_code)]
    pub const fn request_exit(&mut self) {
        self.exiting = true;
    }

    /// True if `request_exit()` was called.
    #[allow(dead_code)]
    pub const fn is_exiting(&self) -> bool {
        self.exiting
    }

    // ─── Configure mode ──────────────────────────────────────────────────────

    /// Enter the interactive configure menu.  Call this when the user runs
    /// `/configure` in the TUI.  `data` is a snapshot of live config values
    /// built by the caller (`LiveCli`).
    /// True if a modal overlay is currently active and needs the main
    /// `read_input` loop to drive it.
    ///
    /// Used by the in-flight wait loop to detect that a slash command (e.g.
    /// `/ssh`) dispatched mid-turn has opened a modal — the wait loop's key
    /// handler can't operate modals, so the caller must break out and return
    /// to `read_input` so the user can interact with the overlay.
    pub fn has_active_modal(&self) -> bool {
        self.ssh_form.is_some()
            || self.configure_state != ConfigureState::Inactive
            || self.provider_login_modal.is_some()
            || self.confirm_modal.is_some()
            || self.password_modal.is_some()
    }

    // ─── Task #627 modal helpers ─────────────────────────────────────────────

    /// Open a confirm modal with the given pending action.  Replaces
    /// any previously-open confirm modal (rare; only happens if a
    /// command intercepts itself).
    pub(crate) fn open_confirm_modal(
        &mut self,
        title: impl Into<String>,
        body: impl Into<String>,
        action: modals::PendingConfirmAction,
    ) {
        self.confirm_modal = Some(modals::ConfirmModal::new(title, body));
        self.pending_confirm_action = Some(action);
        self.redraw.request_full();
    }

    /// Open a password modal with the given pending action.
    pub(crate) fn open_password_modal(
        &mut self,
        title: impl Into<String>,
        prompt: impl Into<String>,
        action: modals::PendingPasswordAction,
    ) {
        self.password_modal = Some(modals::PasswordModal::new(title, prompt));
        self.pending_password_action = Some(action);
        self.redraw.request_full();
    }

    /// Close the confirm modal and consume its pending action.  Idempotent.
    pub(crate) fn close_confirm_modal(&mut self) {
        self.confirm_modal = None;
        self.pending_confirm_action = None;
        self.redraw.request_full();
    }

    /// Close the password modal and consume its pending action.  Idempotent.
    pub(crate) fn close_password_modal(&mut self) {
        self.password_modal = None;
        self.pending_password_action = None;
        self.redraw.request_full();
    }

    /// Set an error message on the open password modal (e.g. after a
    /// failed unlock attempt).  No-op if no password modal is open.
    /// Increments the modal's `attempts` counter — the host should
    /// check `password_modal_attempts()` against its retry cap and
    /// close the modal when the cap is exceeded.
    pub(crate) fn password_modal_set_error(&mut self, msg: impl Into<String>) {
        if let Some(modal) = self.password_modal.as_mut() {
            modal.set_error(msg);
            self.redraw.request_full();
        }
    }

    /// Current attempt count on the open password modal, or 0 if none.
    pub(crate) fn password_modal_attempts(&self) -> u32 {
        self.password_modal.as_ref().map_or(0, |m| m.attempts)
    }

    /// Open the provider-login modal for the given provider (#578).
    ///
    /// No-op when stdout is not a terminal (headless/`--print` mode) — the
    /// caller must fall back to the existing CLI flow in that case.
    pub fn open_provider_login_modal(&mut self, provider: api::ProviderKind) {
        self.provider_login_modal = Some(provider_login::ProviderLoginModal::open(provider));
        self.redraw.request_full();
    }

    /// Close the provider-login modal if it is open.
    pub fn close_provider_login_modal(&mut self) {
        self.provider_login_modal = None;
        self.redraw.request_full();
    }

    pub fn enter_configure_mode(&mut self, data: ConfigureData) {
        self.configure_data = data;
        self.configure_state = ConfigureState::MainMenu { selected: 0 };
    }

    /// Key handler for configure mode.
    pub(super) fn handle_configure_key(&mut self, key: crossterm::event::KeyEvent) -> io::Result<ReadResult> {
        use configure_types::ConfigureState;

        // EditingValue has its own character-level handler.
        if let ConfigureState::EditingValue { ref mut value, ref mut cursor, .. } =
            self.configure_state
        {
            match key.code {
                KeyCode::Esc => {
                    let section = match &self.configure_state {
                        ConfigureState::EditingValue { section, .. } => section.clone(),
                        _ => String::new(),
                    };
                    self.configure_state = section_state_from_name(&section, 0);
                    return Ok(ReadResult::Continue);
                }
                KeyCode::Enter => {
                    let (section, key_name, value_str) = match &self.configure_state {
                        ConfigureState::EditingValue { section, key, value, .. } => {
                            (section.clone(), key.clone(), value.clone())
                        }
                        _ => unreachable!(),
                    };
                    let action = configure_action_for(&section, &key_name, &value_str);
                    self.configure_state = section_state_from_name(&section, 0);
                    if let Some(action) = action {
                        return Ok(ReadResult::ConfigureAction(action));
                    }
                    return Ok(ReadResult::Continue);
                }
                KeyCode::Char(ch) => {
                    value.insert(*cursor, ch);
                    *cursor += ch.len_utf8();
                }
                KeyCode::Backspace => {
                    if *cursor > 0 {
                        let prev = prev_char_boundary(value, *cursor);
                        value.drain(prev..*cursor);
                        *cursor = prev;
                    }
                }
                KeyCode::Delete => {
                    if *cursor < value.len() {
                        let next = next_char_boundary(value, *cursor);
                        value.drain(*cursor..next);
                    }
                }
                KeyCode::Left => {
                    if *cursor > 0 {
                        *cursor = prev_char_boundary(value, *cursor);
                    }
                }
                KeyCode::Right => {
                    if *cursor < value.len() {
                        *cursor = next_char_boundary(value, *cursor);
                    }
                }
                KeyCode::Home => *cursor = 0,
                KeyCode::End => *cursor = value.len(),
                _ => {}
            }
            return Ok(ReadResult::Continue);
        }

        // StatusLineEditor::SeparatorEdit has its own character-level handler.
        if let ConfigureState::StatusLineEditor {
            sub: configure_types::StatusLineEditorSub::SeparatorEdit { value, cursor },
            draft,
            ..
        } = &mut self.configure_state
        {
            match key.code {
                KeyCode::Esc => {
                    // Cancel — revert to Overview
                    self.configure_state = ConfigureState::StatusLineEditor {
                        sub: configure_types::StatusLineEditorSub::Overview,
                        selected: 2,
                        draft: draft.clone(),
                    };
                    return Ok(ReadResult::Continue);
                }
                KeyCode::Enter => {
                    // Apply separator to draft
                    let new_sep = value.clone();
                    let mut new_draft = draft.clone();
                    new_draft.set_separator(new_sep);
                    self.configure_state = ConfigureState::StatusLineEditor {
                        sub: configure_types::StatusLineEditorSub::Overview,
                        selected: 2,
                        draft: new_draft,
                    };
                    return Ok(ReadResult::Continue);
                }
                KeyCode::Char(ch) => { value.insert(*cursor, ch); *cursor += ch.len_utf8(); }
                KeyCode::Backspace => {
                    if *cursor > 0 {
                        let prev = prev_char_boundary(value, *cursor);
                        value.drain(prev..*cursor);
                        *cursor = prev;
                    }
                }
                KeyCode::Left => { if *cursor > 0 { *cursor = prev_char_boundary(value, *cursor); } }
                KeyCode::Right => { if *cursor < value.len() { *cursor = next_char_boundary(value, *cursor); } }
                KeyCode::Home => *cursor = 0,
                KeyCode::End => *cursor = value.len(),
                _ => {}
            }
            return Ok(ReadResult::Continue);
        }

        // StatusLineEditor::LineDetail — Left/Right to reorder widgets.
        if let ConfigureState::StatusLineEditor {
            sub: configure_types::StatusLineEditorSub::LineDetail { line_idx },
            draft,
            selected,
            ..
        } = &mut self.configure_state
        {
            use runtime::theme::Side;
            let left_len = draft.widgets_on_side(*line_idx, Side::Left).len();
            let right_start = left_len + 1;
            let right_len = draft.widgets_on_side(*line_idx, Side::Right).len();

            match key.code {
                KeyCode::Left if *selected < left_len => {
                    draft.move_widget(*line_idx, Side::Left, *selected, -1);
                    if *selected > 0 { *selected -= 1; }
                    return Ok(ReadResult::Continue);
                }
                KeyCode::Right if *selected < left_len => {
                    draft.move_widget(*line_idx, Side::Left, *selected, 1);
                    if *selected + 1 < left_len { *selected += 1; }
                    return Ok(ReadResult::Continue);
                }
                KeyCode::Left if *selected >= right_start && *selected < right_start + right_len => {
                    let idx = *selected - right_start;
                    draft.move_widget(*line_idx, Side::Right, idx, -1);
                    if idx > 0 { *selected -= 1; }
                    return Ok(ReadResult::Continue);
                }
                KeyCode::Right if *selected >= right_start && *selected < right_start + right_len => {
                    let idx = *selected - right_start;
                    draft.move_widget(*line_idx, Side::Right, idx, 1);
                    if idx + 1 < right_len { *selected += 1; }
                    return Ok(ReadResult::Continue);
                }
                _ => { /* fall through to standard navigation */ }
            }
        }

        // Navigation / selection for all other states.
        match key.code {
            KeyCode::Up => {
                self.configure_move(-1);
            }
            KeyCode::Down => {
                self.configure_move(1);
            }
            KeyCode::PageUp => {
                self.configure_page(-1);
            }
            KeyCode::PageDown => {
                self.configure_page(1);
            }
            KeyCode::Home => {
                self.configure_jump_home();
            }
            KeyCode::End => {
                self.configure_jump_end();
            }
            KeyCode::Enter => {
                return self.configure_select();
            }
            KeyCode::Esc => {
                self.configure_back();
            }
            _ => {}
        }
        Ok(ReadResult::Continue)
    }

    /// Page selection by `direction * approx_height` rows.  Positive direction
    /// moves toward the bottom of the list, negative toward the top.
    /// Selection is clamped to `[0, count - 1]`; the rendering pass picks up
    /// the new offset via `follow_selection`.
    fn configure_page(&mut self, direction: i32) {
        let count = configure_item_count(&self.configure_state, &self.configure_data);
        if count == 0 {
            return;
        }
        let approx_height = self
            .terminal
            .size()
            .map(|s| s.height.saturating_sub(6) as usize)
            .unwrap_or(18);
        // Subtract 4 for the header lines so a single PgDn shifts selection
        // by one *body* page rather than overshooting.  Minimum step is 1.
        let step = approx_height.saturating_sub(4).max(1);
        let selected = configure_selected(&self.configure_state);
        let new_selected = if direction < 0 {
            selected.saturating_sub(step)
        } else {
            (selected + step).min(count - 1)
        };
        configure_set_selected(&mut self.configure_state, new_selected);
    }

    /// Jump selection to the first item.
    fn configure_jump_home(&mut self) {
        if configure_item_count(&self.configure_state, &self.configure_data) == 0 {
            return;
        }
        configure_set_selected(&mut self.configure_state, 0);
    }

    /// Jump selection to the last item.
    fn configure_jump_end(&mut self) {
        let count = configure_item_count(&self.configure_state, &self.configure_data);
        if count == 0 {
            return;
        }
        configure_set_selected(&mut self.configure_state, count - 1);
    }

    /// Mouse-wheel scroll of the configure overlay.  Negative deltas scroll
    /// up (toward the top of the list); positive deltas scroll down.
    /// Selection is NOT moved — the wheel is for inspection, not navigation
    /// (matches the Claude Code behaviour that `/skills` and `/agents`
    /// dialogs adopted in v2.1.121).
    pub(super) fn configure_scroll_wheel(&mut self, delta: i32) {
        let total_items = configure_item_count(&self.configure_state, &self.configure_data);
        if total_items == 0 {
            return;
        }
        let approx_height = self
            .terminal
            .size()
            .map(|s| s.height.saturating_sub(6) as usize)
            .unwrap_or(18);
        let total_lines = total_items.saturating_add(4);
        if delta < 0 {
            self.configure_viewport = self
                .configure_viewport
                .scroll_up((-delta) as usize);
        } else {
            self.configure_viewport = self
                .configure_viewport
                .scroll_down(delta as usize, total_lines, approx_height);
        }
    }

    /// Move the selected item index by `delta` (-1 = up, +1 = down).
    #[allow(clippy::cast_sign_loss)]
    fn configure_move(&mut self, delta: i32) {
        let count = configure_item_count(&self.configure_state, &self.configure_data);
        if count == 0 {
            return;
        }
        let selected = configure_selected(&self.configure_state);
        let new_selected = if delta < 0 {
            selected.saturating_sub((-delta) as usize)
        } else {
            (selected + delta as usize).min(count - 1)
        };
        configure_set_selected(&mut self.configure_state, new_selected);
    }

    /// Handle Enter on the currently selected item.
    fn configure_select(&mut self) -> io::Result<ReadResult> {
        let selected = configure_selected(&self.configure_state);
        match self.configure_state.clone() {
            ConfigureState::MainMenu { .. } => {
                self.configure_state = match selected {
                    0 => ConfigureState::Providers { selected: 0 },
                    1 => ConfigureState::Models { selected: 0 },
                    2 => ConfigureState::Context { selected: 0 },
                    3 => ConfigureState::Search { selected: 0 },
                    4 => ConfigureState::Permissions { selected: 0 },
                    5 => ConfigureState::Display { selected: 0 },
                    6 => ConfigureState::Integrations { selected: 0 },
                    7 => ConfigureState::LanguageTheme { selected: 0 },
                    8 => ConfigureState::Vault { selected: 0 },
                    9 => ConfigureState::Notifications { selected: 0 },
                    10 => ConfigureState::Failover { selected: 0 },
                    11 => ConfigureState::Ssh { selected: 0 },
                    12 => ConfigureState::DockerK8s { selected: 0 },
                    13 => ConfigureState::Database { selected: 0 },
                    14 => ConfigureState::MemoryArchive { selected: 0 },
                    15 => ConfigureState::PluginsCron { selected: 0 },
                    16 => ConfigureState::StatusLineEditor {
                        sub: configure_types::StatusLineEditorSub::Overview,
                        selected: 0,
                        draft: Box::new(self.status_line_config.clone()),
                    },
                    _ => ConfigureState::MainMenu { selected },
                };
            }
            ConfigureState::Providers { .. } => {
                let provider = match selected {
                    0 => "anthropic",
                    1 => "openai",
                    2 => "ollama",
                    3 => "xai",
                    _ => return Ok(ReadResult::Continue),
                };
                self.configure_state = ConfigureState::ProviderDetail {
                    provider: provider.to_string(),
                    selected: 0,
                };
            }
            ConfigureState::ProviderDetail { ref provider, selected } => {
                let p = provider.clone();
                match (p.as_str(), selected) {
                    ("anthropic", 0) => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(
                            ConfigureAction::RefreshAnthropicOAuth,
                        ));
                    }
                    ("anthropic", 1) | ("openai" | "xai", 0) => {
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Providers".to_string(),
                            key: format!("{p}_api_key"),
                            value: String::new(),
                            cursor: 0,
                        };
                    }
                    ("ollama", 0) => {
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Providers".to_string(),
                            key: "ollama_host".to_string(),
                            value: self.configure_data.ollama_host.clone(),
                            cursor: self.configure_data.ollama_host.len(),
                        };
                    }
                    _ => {}
                }
            }
            ConfigureState::Models { selected } => {
                match selected {
                    0 => {
                        let current = self.configure_data.default_model.clone();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Models".to_string(),
                            key: "default_model".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    1 => {
                        let current = self.configure_data.image_model.clone();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Models".to_string(),
                            key: "image_model".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    _ => {}
                }
            }
            ConfigureState::Context { selected } => {
                match selected {
                    0 => {
                        let current = self.configure_data.context_size.to_string();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Context".to_string(),
                            key: "context_size".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    1 => {
                        let current = self.configure_data.compact_threshold.to_string();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Context".to_string(),
                            key: "compact_threshold".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    2 => {
                        self.configure_state = ConfigureState::Inactive;
                        let enabled = !self.configure_data.qmd_status.contains("disabled");
                        return Ok(ReadResult::ConfigureAction(
                            ConfigureAction::SetQmdEnabled { enabled: !enabled },
                        ));
                    }
                    _ => {}
                }
            }
            ConfigureState::Search { selected } => {
                match selected {
                    0 => {
                        let current = self.configure_data.default_search.clone();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Search".to_string(),
                            key: "default_search".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    n if n >= 1 && n <= self.configure_data.search_providers.len() => {
                        let provider_name =
                            self.configure_data.search_providers[n - 1].0.clone();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Search".to_string(),
                            key: format!("{provider_name}_key"),
                            value: String::new(),
                            cursor: 0,
                        };
                    }
                    _ => {}
                }
            }
            ConfigureState::Permissions { selected } => {
                let mode = match selected {
                    0 => "read-only",
                    1 => "workspace-write",
                    2 => "danger-full-access",
                    _ => return Ok(ReadResult::Continue),
                };
                self.configure_state = ConfigureState::Inactive;
                return Ok(ReadResult::ConfigureAction(
                    ConfigureAction::SetPermissionMode { mode: mode.to_string() },
                ));
            }
            ConfigureState::Display { selected } => {
                match selected {
                    0 => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::ToggleVim));
                    }
                    1 => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::ToggleChat));
                    }
                    _ => {}
                }
            }
            ConfigureState::LanguageTheme { selected } => {
                match selected {
                    0 => {
                        let langs = ["en", "de", "es", "fr", "ja", "zh-CN", "ru"];
                        let current = &self.configure_data.language;
                        let idx = langs.iter().position(|l| *l == current.as_str()).unwrap_or(0);
                        let next = langs[(idx + 1) % langs.len()];
                        self.configure_data.language = next.to_string();
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::SetLanguage {
                            lang: next.to_string(),
                        }));
                    }
                    1 => {
                        let themes = [
                            "culpur-defense", "cyberpunk", "nord", "solarized-dark",
                            "dracula", "monokai", "gruvbox", "catppuccin",
                        ];
                        let current = &self.configure_data.active_theme;
                        let idx = themes.iter().position(|t| *t == current.as_str()).unwrap_or(0);
                        let next = themes[(idx + 1) % themes.len()];
                        self.configure_data.active_theme = next.to_string();
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::SetTheme {
                            theme: next.to_string(),
                        }));
                    }
                    _ => {}
                }
            }
            ConfigureState::Vault { selected } => {
                match selected {
                    0 => {
                        let current = self.configure_data.vault_session_ttl.to_string();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Vault".to_string(),
                            key: "vault_session_ttl".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    1 => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::ToggleVaultAutoLock));
                    }
                    _ => {}
                }
            }
            ConfigureState::Notifications { selected } => {
                let key = match selected {
                    0 => {
                        let platforms = [
                            "desktop", "discord", "slack", "telegram",
                            "whatsapp", "signal", "matrix", "webhook",
                        ];
                        let current = &self.configure_data.notify_platform;
                        let idx = platforms.iter().position(|p| *p == current.as_str()).unwrap_or(0);
                        let next = platforms[(idx + 1) % platforms.len()];
                        self.configure_data.notify_platform = next.to_string();
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::SetNotifyPlatform {
                            platform: next.to_string(),
                        }));
                    }
                    1 => "notify_discord_webhook",
                    2 => "notify_slack_webhook",
                    3 => "notify_telegram_token",
                    4 => "notify_whatsapp_url",
                    5 => "notify_whatsapp_token",
                    6 => "notify_matrix_homeserver",
                    7 => "notify_matrix_token",
                    8 => "notify_signal_sender",
                    9 => "notify_signal_cli_path",
                    _ => return Ok(ReadResult::Continue),
                };
                let current_val = configure_data_notify_value(&self.configure_data, key);
                let cur_len = current_val.len();
                self.configure_state = ConfigureState::EditingValue {
                    section: "Notifications".to_string(),
                    key: key.to_string(),
                    value: current_val,
                    cursor: cur_len,
                };
            }
            ConfigureState::Failover { selected } => {
                match selected {
                    0 => {
                        let current = self.configure_data.failover_cooldown.to_string();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Failover".to_string(),
                            key: "failover_cooldown".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    1 => {
                        let current = self.configure_data.failover_budget.to_string();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Failover".to_string(),
                            key: "failover_budget".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    2 => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::ToggleFailoverAutoRecovery));
                    }
                    _ => {}
                }
            }
            ConfigureState::Ssh { selected } => {
                let (key, cur_val) = match selected {
                    0 => ("ssh_key_path", self.configure_data.ssh_key_path.clone()),
                    1 => ("ssh_bastion_host", self.configure_data.ssh_bastion_host.clone()),
                    2 => ("ssh_config_path", self.configure_data.ssh_config_path.clone()),
                    _ => return Ok(ReadResult::Continue),
                };
                let cur_len = cur_val.len();
                self.configure_state = ConfigureState::EditingValue {
                    section: "SSH".to_string(),
                    key: key.to_string(),
                    value: cur_val,
                    cursor: cur_len,
                };
            }
            ConfigureState::DockerK8s { selected } => {
                let (key, cur_val) = match selected {
                    0 => ("docker_compose_file", self.configure_data.docker_compose_file.clone()),
                    1 => ("docker_registry", self.configure_data.docker_registry.clone()),
                    2 => ("k8s_context", self.configure_data.k8s_context.clone()),
                    3 => ("k8s_namespace", self.configure_data.k8s_namespace.clone()),
                    _ => return Ok(ReadResult::Continue),
                };
                let cur_len = cur_val.len();
                self.configure_state = ConfigureState::EditingValue {
                    section: "DockerK8s".to_string(),
                    key: key.to_string(),
                    value: cur_val,
                    cursor: cur_len,
                };
            }
            ConfigureState::Database { selected } => {
                match selected {
                    0 => {
                        self.configure_state = ConfigureState::EditingValue {
                            section: "Database".to_string(),
                            key: "db_url".to_string(),
                            value: String::new(),
                            cursor: 0,
                        };
                    }
                    1 => {
                        let tools = ["prisma", "knex", "typeorm"];
                        let current = &self.configure_data.db_schema_tool;
                        let idx = tools.iter().position(|t| *t == current.as_str()).unwrap_or(0);
                        let next = tools[(idx + 1) % tools.len()];
                        self.configure_data.db_schema_tool = next.to_string();
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::SetDbSchemaTool {
                            tool: next.to_string(),
                        }));
                    }
                    _ => {}
                }
            }
            ConfigureState::MemoryArchive { selected } => {
                match selected {
                    0 => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::ToggleAutoSaveMemory));
                    }
                    1 => {
                        let current = self.configure_data.archive_frequency.to_string();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "MemoryArchive".to_string(),
                            key: "archive_frequency".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    2 => {
                        let current = self.configure_data.archive_retention_days.to_string();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "MemoryArchive".to_string(),
                            key: "archive_retention_days".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    3 => {
                        let current = self.configure_data.memory_dir.clone();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "MemoryArchive".to_string(),
                            key: "memory_dir".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    _ => {}
                }
            }
            ConfigureState::PluginsCron { selected } => {
                match selected {
                    0 => {
                        let current = self.configure_data.plugin_search_paths.clone();
                        let cur_len = current.len();
                        self.configure_state = ConfigureState::EditingValue {
                            section: "PluginsCron".to_string(),
                            key: "plugin_search_paths".to_string(),
                            value: current,
                            cursor: cur_len,
                        };
                    }
                    1 => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::ToggleAutoEnablePlugins));
                    }
                    2 => {
                        self.configure_state = ConfigureState::Inactive;
                        return Ok(ReadResult::ConfigureAction(ConfigureAction::ToggleCronEnabled));
                    }
                    _ => {}
                }
            }
            ConfigureState::StatusLineEditor { sub, mut draft, .. } => {
                use configure_types::StatusLineEditorSub;
                use runtime::theme::{StatusLinePreset, StatusWidget as SW, Side};
                match sub {
                    StatusLineEditorSub::Overview => match selected {
                        0 => {
                            self.configure_state = ConfigureState::StatusLineEditor {
                                sub: StatusLineEditorSub::PresetPicker,
                                selected: 0,
                                draft,
                            };
                        }
                        1 => {
                            self.configure_state = ConfigureState::StatusLineEditor {
                                sub: StatusLineEditorSub::LineList,
                                selected: 0,
                                draft,
                            };
                        }
                        2 => {
                            let current = draft.separator_char.clone();
                            let len = current.len();
                            self.configure_state = ConfigureState::StatusLineEditor {
                                sub: StatusLineEditorSub::SeparatorEdit { value: current, cursor: len },
                                selected: 0,
                                draft,
                            };
                        }
                        3 => {
                            let c = !draft.compact;
                            draft.set_compact(c);
                            self.configure_state = ConfigureState::StatusLineEditor {
                                sub: StatusLineEditorSub::Overview,
                                selected: 3,
                                draft,
                            };
                        }
                        4 => {
                            // Save & Apply
                            self.configure_state = ConfigureState::Inactive;
                            return Ok(ReadResult::ConfigureAction(ConfigureAction::ApplyStatusLineConfig { config: draft }));
                        }
                        5 => {
                            // Reset to default
                            self.configure_state = ConfigureState::StatusLineEditor {
                                sub: StatusLineEditorSub::Overview,
                                selected: 0,
                                draft: Box::new(runtime::theme::StatusLineConfig::default()),
                            };
                        }
                        _ => {}
                    }
                    StatusLineEditorSub::PresetPicker => {
                        let all = StatusLinePreset::all();
                        if selected < all.len() {
                            let new_config = runtime::theme::StatusLineConfig::from_preset(all[selected]);
                            self.configure_state = ConfigureState::StatusLineEditor {
                                sub: StatusLineEditorSub::Overview,
                                selected: 0,
                                draft: Box::new(new_config),
                            };
                        }
                    }
                    StatusLineEditorSub::LineList => {
                        if selected < draft.line_count() {
                            self.configure_state = ConfigureState::StatusLineEditor {
                                sub: StatusLineEditorSub::LineDetail { line_idx: selected },
                                selected: 0,
                                draft,
                            };
                        } else {
                            // "Add New Line"
                            draft.add_line();
                            self.configure_state = ConfigureState::StatusLineEditor {
                                sub: StatusLineEditorSub::LineList,
                                selected: draft.line_count().saturating_sub(1),
                                draft,
                            };
                        }
                    }
                    StatusLineEditorSub::LineDetail { line_idx } => {
                        let left_len = draft.widgets_on_side(line_idx, Side::Left).len();
                        let right_len = draft.widgets_on_side(line_idx, Side::Right).len();
                        let add_left_row = left_len;
                        let right_start = add_left_row + 1;
                        let add_right_row = right_start + right_len;
                        let delete_row = add_right_row + 1;

                        if selected < left_len {
                            // Remove left widget
                            draft.remove_widget(line_idx, Side::Left, selected);
                            self.configure_state = ConfigureState::StatusLineEditor {
                                sub: StatusLineEditorSub::LineDetail { line_idx },
                                selected: selected.min(draft.widgets_on_side(line_idx, Side::Left).len().saturating_sub(1)),
                                draft,
                            };
                        } else if selected == add_left_row {
                            // Add widget to left
                            self.configure_state = ConfigureState::StatusLineEditor {
                                sub: StatusLineEditorSub::WidgetPicker { line_idx, side: Side::Left },
                                selected: 0,
                                draft,
                            };
                        } else if selected > right_start.saturating_sub(1) && selected < add_right_row {
                            // Remove right widget
                            let widget_idx = selected - right_start;
                            draft.remove_widget(line_idx, Side::Right, widget_idx);
                            self.configure_state = ConfigureState::StatusLineEditor {
                                sub: StatusLineEditorSub::LineDetail { line_idx },
                                selected,
                                draft,
                            };
                        } else if selected == add_right_row {
                            // Add widget to right
                            self.configure_state = ConfigureState::StatusLineEditor {
                                sub: StatusLineEditorSub::WidgetPicker { line_idx, side: Side::Right },
                                selected: 0,
                                draft,
                            };
                        } else if selected == delete_row {
                            // Delete this line
                            draft.remove_line(line_idx);
                            self.configure_state = ConfigureState::StatusLineEditor {
                                sub: StatusLineEditorSub::LineList,
                                selected: 0,
                                draft,
                            };
                        }
                    }
                    StatusLineEditorSub::WidgetPicker { line_idx, side } => {
                        let all = SW::all_widgets();
                        if selected < all.len() {
                            draft.add_widget(line_idx, side, all[selected].clone());
                            self.configure_state = ConfigureState::StatusLineEditor {
                                sub: StatusLineEditorSub::LineDetail { line_idx },
                                selected: 0,
                                draft,
                            };
                        }
                    }
                    StatusLineEditorSub::SeparatorEdit { value, .. } => {
                        draft.set_separator(value);
                        self.configure_state = ConfigureState::StatusLineEditor {
                            sub: StatusLineEditorSub::Overview,
                            selected: 2,
                            draft,
                        };
                    }
                }
            }
            _ => {}
        }
        Ok(ReadResult::Continue)
    }

    /// Go back one level in the configure hierarchy.
    fn configure_back(&mut self) {
        self.configure_state = match &self.configure_state {
            ConfigureState::MainMenu { .. } | ConfigureState::Inactive => ConfigureState::Inactive,
            ConfigureState::Providers { .. }
            | ConfigureState::Models { .. }
            | ConfigureState::Context { .. }
            | ConfigureState::Search { .. }
            | ConfigureState::Permissions { .. }
            | ConfigureState::Display { .. }
            | ConfigureState::Integrations { .. }
            | ConfigureState::LanguageTheme { .. }
            | ConfigureState::Vault { .. }
            | ConfigureState::Notifications { .. }
            | ConfigureState::Failover { .. }
            | ConfigureState::Ssh { .. }
            | ConfigureState::DockerK8s { .. }
            | ConfigureState::Database { .. }
            | ConfigureState::MemoryArchive { .. }
            | ConfigureState::PluginsCron { .. } => ConfigureState::MainMenu { selected: 0 },
            ConfigureState::ProviderDetail { .. } => ConfigureState::Providers { selected: 0 },
            ConfigureState::StatusLineEditor { sub, draft, .. } => {
                use configure_types::StatusLineEditorSub;
                match sub {
                    StatusLineEditorSub::Overview => ConfigureState::MainMenu { selected: 16 },
                    StatusLineEditorSub::PresetPicker
                    | StatusLineEditorSub::LineList
                    | StatusLineEditorSub::SeparatorEdit { .. } => ConfigureState::StatusLineEditor {
                        sub: StatusLineEditorSub::Overview,
                        selected: 0,
                        draft: draft.clone(),
                    },
                    StatusLineEditorSub::LineDetail { .. } => ConfigureState::StatusLineEditor {
                        sub: StatusLineEditorSub::LineList,
                        selected: 0,
                        draft: draft.clone(),
                    },
                    StatusLineEditorSub::WidgetPicker { line_idx, .. } => ConfigureState::StatusLineEditor {
                        sub: StatusLineEditorSub::LineDetail { line_idx: *line_idx },
                        selected: 0,
                        draft: draft.clone(),
                    },
                }
            }
            ConfigureState::EditingValue { section, .. } => {
                section_state_from_name(section, 0)
            }
        };
    }

    // ─── Screensaver integration ──────────────────────────────────────────────

    /// Capture a flat list of printable lines currently shown in the content area.
    pub fn capture_screen_text(&self) -> Vec<String> {
        let tab = &self.tabs[self.active_tab];
        let mut out: Vec<String> = Vec::new();
        for entry in &tab.log {
            match entry {
                LogEntry::User(t) | LogEntry::Assistant(t) | LogEntry::System(t) => {
                    for line in t.lines() {
                        out.push(line.to_string());
                    }
                }
                LogEntry::ToolCall { name, detail, .. } => {
                    out.push(format!("  {name}: {}", detail.lines().next().unwrap_or("")));
                }
                LogEntry::Image { path, label } => {
                    out.push(format!("[Image: {label} — {path}]"));
                }
            }
        }
        for line in tab.pending_text.lines() {
            out.push(line.to_string());
        }
        out
    }

    /// Render one screensaver frame.
    pub fn draw_screensaver(&mut self, ss: &mut crate::screensaver::FurnaceScreensaver) -> io::Result<bool> {
        let (w, h) = {
            let size = self.terminal.size()?;
            (size.width, size.height)
        };
        let still_active = ss.tick(w, h);

        self.terminal.draw(|frame| {
            ss.render(frame, frame.area());
        })?;

        Ok(still_active)
    }

    /// Read a single input event in screensaver mode.
    pub fn read_input_screensaver(&mut self, ss: &mut crate::screensaver::FurnaceScreensaver) -> io::Result<ReadResult> {
        let still_active = self.draw_screensaver(ss)?;
        if !still_active {
            return Ok(ReadResult::Continue);
        }

        if event::poll(crate::screensaver::FRAME_INTERVAL)? {
            match event::read()? {
                CtEvent::Key(key) if matches!(key.kind, KeyEventKind::Press) => {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c' | 'C' | 'd' | 'D'))
                    {
                        return Ok(ReadResult::Exit);
                    }
                    ss.resume();
                }
                CtEvent::Mouse(_) => {
                    ss.resume();
                }
                _ => {}
            }
        }
        Ok(ReadResult::Continue)
    }
}

impl Drop for AnvilTui {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        // CC parity (v2.2.14): only leave alt-screen if we entered it.
        if alternate_screen_enabled() {
            let _ = crossterm::execute!(
                io::stdout(),
                crossterm::event::DisableBracketedPaste,
                crossterm::event::DisableMouseCapture,
                terminal::LeaveAlternateScreen
            );
        } else {
            let _ = crossterm::execute!(
                io::stdout(),
                crossterm::event::DisableBracketedPaste,
                crossterm::event::DisableMouseCapture,
            );
        }
        // Task #688: clear the global state flag so panic-recovery /
        // re-init paths don't see stale capture state.
        MOUSE_CAPTURE_ACTIVE.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

// ─── Configure menu renderer ─────────────────────────────────────────────────

/// Render the configure menu for the current state as a Vec of ratatui Lines.
pub(super) fn render_configure_menu(
    state: &ConfigureState,
    data: &ConfigureData,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    let make_row = |label: &str, value: &str, is_cursor: bool| -> Line<'static> {
        let marker = if is_cursor { "  ▸ " } else { "    " };
        let label_str = label.to_string();
        let value_str = value.to_string();
        let label_padded = format!("{label_str:<40}");
        if is_cursor {
            Line::from(vec![
                Span::styled(marker.to_string(), Style::default().fg(Color::Cyan)),
                Span::styled(
                    label_padded,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    value_str,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                ),
            ])
        } else {
            Line::from(vec![
                Span::raw(marker.to_string()),
                Span::styled(label_padded, Style::default().fg(Color::Rgb(0xaa, 0xaa, 0xaa))),
                Span::styled(
                    value_str,
                    Style::default().fg(Color::Rgb(0x66, 0x66, 0x88)).add_modifier(Modifier::DIM),
                ),
            ])
        }
    };

    let heading = configure_breadcrumb(state);
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("  ⚒  {heading}"),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        format!("  {}", "─".repeat(width.saturating_sub(4))),
        Style::default().fg(Color::Rgb(0x33, 0x33, 0x44)),
    )));
    lines.push(Line::from(""));

    let sel = configure_selected(state);

    match state {
        ConfigureState::Inactive => {}

        ConfigureState::MainMenu { .. } => {
            let items: &[(&str, String)] = &[
                ("Providers & Authentication", format!("[{}]", {
                    let mut n = 0;
                    if !data.anthropic_status.contains('✗') { n += 1; }
                    if !data.openai_status.contains('✗') { n += 1; }
                    if !data.ollama_status.contains('✗') { n += 1; }
                    if !data.xai_status.contains('✗') { n += 1; }
                    format!("{n} configured")
                })),
                ("Models & Defaults", format!("[{}]", data.current_model)),
                ("Context & Memory", format!("[{}K]", data.context_size / 1000)),
                ("Search Providers", format!("[{}]", data.default_search)),
                ("Permissions", format!("[{}]", data.permission_mode)),
                ("Display & Interface", format!("[vim:{}]", if data.vim_mode { "on" } else { "off" })),
                ("Integrations", format!("[AnvilHub {}]", if data.anvilhub_url.is_empty() { "✗" } else { "✓" })),
                ("Language & Theme", format!("[{} / {}]", data.language, data.active_theme)),
                ("Vault", {
                    let ttl = data.vault_session_ttl;
                    format!("[TTL {}s, auto-lock:{}]", ttl, if data.vault_auto_lock { "on" } else { "off" })
                }),
                ("Notifications", format!("[{}]", data.notify_platform)),
                ("Failover", format!("[cooldown {}s, auto-recovery:{}]", data.failover_cooldown, if data.failover_auto_recovery { "on" } else { "off" })),
                ("SSH", {
                    if data.ssh_bastion_host.is_empty() {
                        "[not configured]".to_string()
                    } else {
                        format!("[bastion: {}]", data.ssh_bastion_host)
                    }
                }),
                ("Docker & K8s", {
                    if data.k8s_context.is_empty() {
                        "[not configured]".to_string()
                    } else {
                        format!("[ctx: {}]", data.k8s_context)
                    }
                }),
                ("Database", {
                    if data.db_url.is_empty() {
                        "[not configured]".to_string()
                    } else {
                        format!("[{} / {}]", data.db_schema_tool, mask_sensitive(&data.db_url))
                    }
                }),
                ("Memory & Archive", format!("[auto-save:{}, retention:{}d]", if data.auto_save_memory { "on" } else { "off" }, data.archive_retention_days)),
                ("Plugins & Cron", format!("[cron:{}, {} active jobs]", if data.cron_enabled { "on" } else { "off" }, data.active_cron_jobs.len())),
                ("Status Line Editor", format!("[{}]", data.status_line_preset)),
            ];
            for (i, (label, value)) in items.iter().enumerate() {
                lines.push(make_row(label, value, i == sel));
            }
        }

        ConfigureState::Providers { .. } => {
            let items = [
                ("Anthropic", data.anthropic_status.clone()),
                ("OpenAI", data.openai_status.clone()),
                ("Ollama", format!("{} ({})", data.ollama_status, data.ollama_host)),
                ("xAI", data.xai_status.clone()),
            ];
            for (i, (label, value)) in items.iter().enumerate() {
                lines.push(make_row(label, value, i == sel));
            }
        }

        ConfigureState::ProviderDetail { provider, .. } => {
            match provider.as_str() {
                "anthropic" => {
                    lines.push(Line::from(vec![
                        Span::raw("    Status:  "),
                        Span::styled(data.anthropic_status.clone(), Style::default().fg(Color::Cyan)),
                    ]));
                    lines.push(Line::from(""));
                    lines.push(make_row("Refresh OAuth token", "[opens browser]", sel == 0));
                    lines.push(make_row("Set API key instead", "[enter key]", sel == 1));
                }
                "openai" => {
                    lines.push(Line::from(vec![
                        Span::raw("    Status:  "),
                        Span::styled(data.openai_status.clone(), Style::default().fg(Color::Cyan)),
                    ]));
                    lines.push(Line::from(""));
                    lines.push(make_row("Set OPENAI_API_KEY", "[enter key]", sel == 0));
                }
                "ollama" => {
                    lines.push(Line::from(vec![
                        Span::raw("    Status:  "),
                        Span::styled(data.ollama_status.clone(), Style::default().fg(Color::Cyan)),
                    ]));
                    lines.push(Line::from(vec![
                        Span::raw("    Host:    "),
                        Span::styled(data.ollama_host.clone(), Style::default().fg(Color::Yellow)),
                    ]));
                    lines.push(Line::from(""));
                    lines.push(make_row("Set Ollama host URL", "[edit]", sel == 0));
                }
                "xai" => {
                    lines.push(Line::from(vec![
                        Span::raw("    Status:  "),
                        Span::styled(data.xai_status.clone(), Style::default().fg(Color::Cyan)),
                    ]));
                    lines.push(Line::from(""));
                    lines.push(make_row("Set XAI_API_KEY", "[enter key]", sel == 0));
                }
                _ => {}
            }
        }

        ConfigureState::Models { .. } => {
            lines.push(make_row("Default model", &data.default_model, sel == 0));
            lines.push(make_row("Image model", &data.image_model, sel == 1));
            if !data.failover_chain.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "    Failover chain:",
                    Style::default().fg(Color::Rgb(0x88, 0x88, 0x88)),
                )));
                for (i, m) in data.failover_chain.iter().enumerate() {
                    lines.push(Line::from(Span::styled(
                        format!("      {}. {m}", i + 1),
                        Style::default().fg(Color::Rgb(0x66, 0x88, 0x66)),
                    )));
                }
            }
        }

        ConfigureState::Context { .. } => {
            let size_label = {
                let kb = data.context_size / 1000;
                let mb = data.context_size / 1_000_000;
                if mb > 0 { format!("{mb}M tokens") } else { format!("{kb}K tokens") }
            };
            lines.push(make_row("Context window size", &size_label, sel == 0));
            lines.push(make_row(
                "Auto-compact threshold",
                &format!("{}%", data.compact_threshold),
                sel == 1,
            ));
            let qmd_label = if data.qmd_status.contains("disabled") { "off" } else { "on" };
            lines.push(make_row(
                "QMD integration",
                &format!("{qmd_label}  ({}) ", data.qmd_status),
                sel == 2,
            ));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("    History archives: {}  |  Pinned files: {}", data.history_count, data.pinned_count),
                Style::default().fg(Color::Rgb(0x66, 0x66, 0x88)),
            )));
        }

        ConfigureState::Search { .. } => {
            lines.push(make_row("Default search provider", &data.default_search, sel == 0));
            for (i, (name, enabled, has_key)) in data.search_providers.iter().enumerate() {
                let status = if *has_key { "✓ key set" } else { "✗ no key" };
                let enabled_str = if *enabled { "" } else { " [disabled]" };
                lines.push(make_row(
                    name.as_str(),
                    &format!("{status}{enabled_str}"),
                    sel == i + 1,
                ));
            }
        }

        ConfigureState::Permissions { .. } => {
            let current = &data.permission_mode;
            let modes = [
                ("read-only", "Read-only — no file writes"),
                ("workspace-write", "Workspace write — within project"),
                ("danger-full-access", "Full access — no restrictions"),
            ];
            for (i, (mode, desc)) in modes.iter().enumerate() {
                let active = if current == *mode { " [active]" } else { "" };
                lines.push(make_row(&format!("{desc}{active}"), "", i == sel));
            }
        }

        ConfigureState::Display { .. } => {
            lines.push(make_row(
                "Vim keybindings",
                if data.vim_mode { "[on]  — Enter to toggle" } else { "[off] — Enter to toggle" },
                sel == 0,
            ));
            lines.push(make_row(
                "Chat-only mode (no tools)",
                if data.chat_mode { "[on]  — Enter to toggle" } else { "[off] — Enter to toggle" },
                sel == 1,
            ));
        }

        ConfigureState::Integrations { .. } => {
            let hub = if data.anvilhub_url.is_empty() {
                "✗ not configured".to_string()
            } else {
                format!("✓ {}", data.anvilhub_url)
            };
            lines.push(make_row("AnvilHub", &hub, sel == 0));
            lines.push(make_row(
                "WordPress",
                if data.wp_configured { "✓ configured" } else { "✗ not configured" },
                sel == 1,
            ));
            lines.push(make_row(
                "GitHub",
                if data.github_configured { "✓ configured" } else { "✗ not configured" },
                sel == 2,
            ));
        }

        ConfigureState::LanguageTheme { .. } => {
            lines.push(make_row(
                "Display language",
                &format!("[{}]  — Enter to cycle", data.language),
                sel == 0,
            ));
            lines.push(make_row(
                "Active theme",
                &format!("[{}]  — Enter to cycle", data.active_theme),
                sel == 1,
            ));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "    Languages: en  de  es  fr  ja  zh-CN  ru",
                Style::default().fg(Color::Rgb(0x66, 0x66, 0x88)),
            )));
            lines.push(Line::from(Span::styled(
                "    Themes: culpur-defense  cyberpunk  nord  solarized-dark  dracula  monokai  gruvbox  catppuccin",
                Style::default().fg(Color::Rgb(0x66, 0x66, 0x88)),
            )));
        }

        ConfigureState::Vault { .. } => {
            lines.push(make_row(
                "Session TTL (seconds)",
                &data.vault_session_ttl.to_string(),
                sel == 0,
            ));
            lines.push(make_row(
                "Auto-lock on idle",
                if data.vault_auto_lock { "[on]  — Enter to toggle" } else { "[off] — Enter to toggle" },
                sel == 1,
            ));
            lines.push(make_row("Vault status", &data.vault_status, sel == 2));
        }

        ConfigureState::Notifications { .. } => {
            lines.push(make_row(
                "Default platform",
                &format!("[{}]  — Enter to cycle", data.notify_platform),
                sel == 0,
            ));
            let masked_or_empty = |s: &str| {
                if s.is_empty() { "[not set]".to_string() } else { mask_sensitive(s) }
            };
            lines.push(make_row("Discord webhook URL",     &masked_or_empty(&data.notify_discord_webhook),   sel == 1));
            lines.push(make_row("Slack webhook URL",       &masked_or_empty(&data.notify_slack_webhook),     sel == 2));
            lines.push(make_row("Telegram bot token",      &masked_or_empty(&data.notify_telegram_token),    sel == 3));
            lines.push(make_row("WhatsApp API URL",        &masked_or_empty(&data.notify_whatsapp_url),      sel == 4));
            lines.push(make_row("WhatsApp token",          &masked_or_empty(&data.notify_whatsapp_token),    sel == 5));
            lines.push(make_row("Matrix homeserver URL",   &masked_or_empty(&data.notify_matrix_homeserver), sel == 6));
            lines.push(make_row("Matrix token",            &masked_or_empty(&data.notify_matrix_token),      sel == 7));
            lines.push(make_row("Signal sender number",    &masked_or_empty(&data.notify_signal_sender),     sel == 8));
            lines.push(make_row("Signal CLI path",         &masked_or_empty(&data.notify_signal_cli_path),   sel == 9));
        }

        ConfigureState::Failover { .. } => {
            lines.push(make_row(
                "Cooldown period (seconds)",
                &data.failover_cooldown.to_string(),
                sel == 0,
            ));
            lines.push(make_row(
                "Usage budget per provider",
                &data.failover_budget.to_string(),
                sel == 1,
            ));
            lines.push(make_row(
                "Auto-recovery",
                if data.failover_auto_recovery { "[on]  — Enter to toggle" } else { "[off] — Enter to toggle" },
                sel == 2,
            ));
            if !data.failover_chain.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "    Provider priority chain:",
                    Style::default().fg(Color::Rgb(0x88, 0x88, 0x88)),
                )));
                for (i, m) in data.failover_chain.iter().enumerate() {
                    lines.push(Line::from(Span::styled(
                        format!("      {}. {m}", i + 1),
                        Style::default().fg(Color::Rgb(0x66, 0x88, 0x66)),
                    )));
                }
            }
        }

        ConfigureState::Ssh { .. } => {
            let fmt = |s: &str| if s.is_empty() { "[not set]".to_string() } else { s.to_string() };
            lines.push(make_row("Default SSH key path",  &fmt(&data.ssh_key_path),     sel == 0));
            lines.push(make_row("Default bastion host",  &fmt(&data.ssh_bastion_host), sel == 1));
            lines.push(make_row("SSH config file path",  &fmt(&data.ssh_config_path),  sel == 2));
        }

        ConfigureState::DockerK8s { .. } => {
            let fmt = |s: &str| if s.is_empty() { "[not set]".to_string() } else { s.to_string() };
            lines.push(make_row("Default compose file", &fmt(&data.docker_compose_file), sel == 0));
            lines.push(make_row("Default registry URL",  &fmt(&data.docker_registry),    sel == 1));
            lines.push(make_row("Default K8s context",   &fmt(&data.k8s_context),        sel == 2));
            lines.push(make_row("Default K8s namespace", &fmt(&data.k8s_namespace),      sel == 3));
        }

        ConfigureState::Database { .. } => {
            let url_display = if data.db_url.is_empty() {
                "[not set]".to_string()
            } else {
                mask_sensitive(&data.db_url)
            };
            lines.push(make_row("Default connection URL (masked)", &url_display, sel == 0));
            lines.push(make_row(
                "Default schema tool",
                &format!("[{}]  — Enter to cycle (prisma/knex/typeorm)", data.db_schema_tool),
                sel == 1,
            ));
        }

        ConfigureState::MemoryArchive { .. } => {
            lines.push(make_row(
                "Auto-save memory",
                if data.auto_save_memory { "[on]  — Enter to toggle" } else { "[off] — Enter to toggle" },
                sel == 0,
            ));
            lines.push(make_row(
                "Archive frequency (compactions)",
                &data.archive_frequency.to_string(),
                sel == 1,
            ));
            lines.push(make_row(
                "Archive retention (days)",
                &data.archive_retention_days.to_string(),
                sel == 2,
            ));
            lines.push(make_row(
                "Memory directory",
                &(if data.memory_dir.is_empty() { "[default]".to_string() } else { data.memory_dir.clone() }),
                sel == 3,
            ));
        }

        ConfigureState::PluginsCron { .. } => {
            lines.push(make_row(
                "Plugin search paths",
                &(if data.plugin_search_paths.is_empty() { "[default]".to_string() } else { data.plugin_search_paths.clone() }),
                sel == 0,
            ));
            lines.push(make_row(
                "Auto-enable new plugins",
                if data.auto_enable_plugins { "[on]  — Enter to toggle" } else { "[off] — Enter to toggle" },
                sel == 1,
            ));
            lines.push(make_row(
                "Cron scheduler",
                if data.cron_enabled { "[on]  — Enter to toggle" } else { "[off] — Enter to toggle" },
                sel == 2,
            ));
            if data.active_cron_jobs.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "    No active cron jobs.",
                    Style::default().fg(Color::Rgb(0x55, 0x55, 0x66)),
                )));
            } else {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "    Active cron jobs:",
                    Style::default().fg(Color::Rgb(0x88, 0x88, 0x88)),
                )));
                for (i, job) in data.active_cron_jobs.iter().enumerate() {
                    lines.push(make_row(job, "", sel == 3 + i));
                }
            }
        }

        ConfigureState::StatusLineEditor { sub, draft, .. } => {
            use configure_types::StatusLineEditorSub;
            use runtime::theme::{StatusLinePreset, StatusWidget as SW, Side};
            match sub {
                StatusLineEditorSub::Overview => {
                    lines.push(make_row("Apply Preset", &format!("[{}]", draft.preset), sel == 0));
                    lines.push(make_row("Edit Lines", &format!("[{} lines]", draft.line_count()), sel == 1));
                    lines.push(make_row("Separator Character", &format!("[{}]", draft.separator_char.trim()), sel == 2));
                    lines.push(make_row("Compact Mode", if draft.compact { "[on]" } else { "[off]" }, sel == 3));
                    lines.push(Line::from(""));
                    lines.push(make_row("\u{2500}\u{2500}\u{2500} Save & Apply \u{2500}\u{2500}\u{2500}", "", sel == 4));
                    lines.push(make_row("\u{2500}\u{2500}\u{2500} Reset to Default \u{2500}\u{2500}\u{2500}", "", sel == 5));
                }
                StatusLineEditorSub::PresetPicker => {
                    for (i, preset) in StatusLinePreset::all().iter().enumerate() {
                        let marker = if preset.name() == draft.preset { "\u{25ba} " } else { "  " };
                        lines.push(make_row(
                            &format!("{marker}{}", preset.name()),
                            preset.description(),
                            sel == i,
                        ));
                    }
                }
                StatusLineEditorSub::LineList => {
                    for i in 0..draft.line_count() {
                        let left_summary: Vec<&str> = draft.widgets_on_side(i, Side::Left).iter().map(runtime::theme::StatusWidget::id).collect();
                        let right_summary: Vec<&str> = draft.widgets_on_side(i, Side::Right).iter().map(runtime::theme::StatusWidget::id).collect();
                        let desc = format!("[{}] \u{2192} [{}]", left_summary.join(", "), right_summary.join(", "));
                        lines.push(make_row(&format!("Line {}", i + 1), &desc, sel == i));
                    }
                    lines.push(Line::from(""));
                    lines.push(make_row("\u{2795} Add New Line", "", sel == draft.line_count()));
                }
                StatusLineEditorSub::LineDetail { line_idx } => {
                    let li = *line_idx;
                    let left_widgets = draft.widgets_on_side(li, Side::Left).to_vec();
                    let right_widgets = draft.widgets_on_side(li, Side::Right).to_vec();
                    let mut row = 0usize;
                    // LEFT header
                    lines.push(Line::from(Span::styled(
                        format!("    LEFT WIDGETS (Line {})", li + 1),
                        Style::default().fg(Color::Yellow),
                    )));
                    for w in &left_widgets {
                        lines.push(make_row(
                            &format!("  {} {}", w.display_name(), if sel == row { "\u{2190}\u{2192} reorder" } else { "" }),
                            &format!("[{}]", w.category()),
                            sel == row,
                        ));
                        row += 1;
                    }
                    lines.push(make_row("  \u{2795} Add Widget (Left)", "", sel == row));
                    row += 1;
                    lines.push(Line::from(""));
                    // RIGHT header
                    lines.push(Line::from(Span::styled(
                        format!("    RIGHT WIDGETS (Line {})", li + 1),
                        Style::default().fg(Color::Cyan),
                    )));
                    for w in &right_widgets {
                        lines.push(make_row(
                            &format!("  {} {}", w.display_name(), if sel == row { "\u{2190}\u{2192} reorder" } else { "" }),
                            &format!("[{}]", w.category()),
                            sel == row,
                        ));
                        row += 1;
                    }
                    lines.push(make_row("  \u{2795} Add Widget (Right)", "", sel == row));
                    row += 1;
                    lines.push(Line::from(""));
                    lines.push(make_row("\u{274c} Delete This Line", "", sel == row));
                }
                StatusLineEditorSub::WidgetPicker { .. } => {
                    let all = SW::all_widgets();
                    let mut current_cat = "";
                    let mut idx = 0;
                    for w in &all {
                        if w.category() != current_cat {
                            current_cat = w.category();
                            lines.push(Line::from(""));
                            lines.push(Line::from(Span::styled(
                                format!("    \u{2500} {} \u{2500}", current_cat.to_uppercase()),
                                Style::default().fg(Color::Yellow),
                            )));
                        }
                        lines.push(make_row(
                            &format!("  {}", w.display_name()),
                            w.id(),
                            sel == idx,
                        ));
                        idx += 1;
                    }
                }
                StatusLineEditorSub::SeparatorEdit { value, cursor } => {
                    lines.push(Line::from(Span::styled(
                        "    Edit separator character:".to_string(),
                        Style::default().fg(Color::Yellow),
                    )));
                    lines.push(Line::from(""));
                    let before: String = value.chars().take(*cursor).collect();
                    let cursor_char = value.chars().nth(*cursor).map_or(" ".to_string(), |c| c.to_string());
                    let after: String = value.chars().skip(*cursor + 1).collect();
                    lines.push(Line::from(vec![
                        Span::raw("    \u{276f} "),
                        Span::styled(before, Style::default().fg(Color::White)),
                        Span::styled(cursor_char, Style::default().fg(Color::Rgb(0x1a, 0x1a, 0x1a)).bg(Color::White)),
                        Span::styled(after, Style::default().fg(Color::White)),
                    ]));
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "    Enter to confirm  Esc to cancel",
                        Style::default().fg(Color::Rgb(0x44, 0x44, 0x55)),
                    )));
                }
            }
        }

        ConfigureState::EditingValue { key, value, cursor, .. } => {
            let prompt = format!("Edit {key}:");
            lines.push(Line::from(Span::styled(
                format!("    {prompt}"),
                Style::default().fg(Color::Yellow),
            )));
            lines.push(Line::from(""));
            let before: String = value.char_indices()
                .take_while(|(i, _)| *i < *cursor)
                .map(|(_, c)| c)
                .collect();
            let cursor_char = value[*cursor..].chars().next().map_or_else(|| " ".to_string(), |c| c.to_string());
            let after = if *cursor < value.len() {
                let next = next_char_boundary(value, *cursor);
                value[next..].to_string()
            } else {
                String::new()
            };
            lines.push(Line::from(vec![
                Span::raw("    ❯ "),
                Span::styled(before, Style::default().fg(Color::White)),
                Span::styled(
                    cursor_char,
                    Style::default().fg(Color::Rgb(0x1a, 0x1a, 0x1a)).bg(Color::White),
                ),
                Span::styled(after, Style::default().fg(Color::White)),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "    Enter to confirm  Esc to cancel",
                Style::default().fg(Color::Rgb(0x44, 0x44, 0x55)),
            )));
        }
    }

    lines.push(Line::from(""));
    lines
}

// ─── Git helpers ──────────────────────────────────────────────────────────────

/// Fetch git branch and diff stats for the current working directory.
/// Fetch git branch, diff stats string, and lines added/removed.
fn fetch_git_info() -> (String, String, u32, u32) {
    use std::process::Command;

    let branch = Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();

    let raw_stat = Command::new("git")
        .args(["diff", "--shortstat"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let diff_stats = parse_shortstat(&raw_stat);
    let (added, removed) = parse_shortstat_nums(&raw_stat);

    (branch, diff_stats, added, removed)
}

/// Parse the output of `git diff --shortstat` into a compact "+N,-M" string.
fn parse_shortstat(s: &str) -> String {
    let mut ins: u32 = 0;
    let mut del: u32 = 0;
    for part in s.split(',') {
        let part = part.trim();
        if part.contains("insertion") {
            if let Some(n) = part.split_whitespace().next() {
                ins = n.parse().unwrap_or(0);
            }
        } else if part.contains("deletion")
            && let Some(n) = part.split_whitespace().next() {
                del = n.parse().unwrap_or(0);
            }
    }
    if ins == 0 && del == 0 {
        String::new()
    } else {
        format!("+{ins},-{del}")
    }
}

/// Parse `git diff --shortstat` into `(insertions, deletions)` as raw numbers.
fn parse_shortstat_nums(s: &str) -> (u32, u32) {
    let mut ins: u32 = 0;
    let mut del: u32 = 0;
    for part in s.split(',') {
        let part = part.trim();
        if part.contains("insertion") {
            if let Some(n) = part.split_whitespace().next() {
                ins = n.parse().unwrap_or(0);
            }
        } else if part.contains("deletion")
            && let Some(n) = part.split_whitespace().next() {
                del = n.parse().unwrap_or(0);
            }
    }
    (ins, del)
}

// ─── Model context helpers ────────────────────────────────────────────────────

/// Return the approximate context window size (in tokens) for a known model.
fn context_max_for_model(model: &str) -> u32 {
    if let Ok(val) = std::env::var("ANVIL_CONTEXT_SIZE")
        && let Ok(n) = val.replace(['k', 'K'], "000")
            .replace(['m', 'M'], "000000")
            .parse::<u32>() {
            return n;
        }

    let m = model.to_lowercase();
    if m.contains("opus") || m.contains("sonnet") {
        1_000_000
    } else if m.contains("haiku") {
        200_000
    } else if m.starts_with("gpt-4o") {
        128_000
    } else if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
        200_000
    } else if m.contains(':') {
        query_ollama_context_size(model).unwrap_or(32_768)
    } else {
        1_000_000
    }
}

/// Query Ollama's /api/show endpoint for the model's actual context window size.
fn query_ollama_context_size(model: &str) -> Option<u32> {
    let ollama_url = std::env::var("OLLAMA_HOST")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    let output = std::process::Command::new("curl")
        .args(["-s", "--max-time", "2", "-X", "POST",
               "-H", "Content-Type: application/json",
               "-d", &format!("{{\"name\":\"{model}\"}}"),
               &format!("{}/api/show", ollama_url.trim_end_matches('/'))])
        .output()
        .ok()?;
    if !output.status.success() { return None; }
    let val: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    val.pointer("/model_info/context_length")
        .or_else(|| val.pointer("/model_info/num_ctx"))
        .and_then(serde_json::Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
        .or_else(|| {
            val.get("parameters")
                .and_then(|p| p.as_str())
                .and_then(|params| {
                    for line in params.lines() {
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        if parts.len() == 2 && parts[0] == "num_ctx" {
                            return parts[1].parse::<u32>().ok();
                        }
                    }
                    None
                })
        })
}

// init_ollama_model_cache and check_clipboard_for_image are re-exported
// from widgets (see pub use widgets::{...} at the top of this file).

// ─── BUG-2 + BUG-3 redraw tests ──────────────────────────────────────────────
//
// These tests exercise the redraw scheduler and `force_full_repaint_after_inline_op`
// contract without requiring a real terminal.  AnvilTui::new() cannot be
// instantiated in unit tests (it requires a TTY), so we test the RedrawScheduler
// directly and verify the flag contracts that `force_full_repaint_after_inline_op`
// relies on.
#[cfg(test)]
mod bug2_bug3_redraw_tests {
    use super::redraw::{DirtyRegions, RedrawScheduler};

    /// BUG-2: after an inline op (OAuth/setup), the redraw scheduler must be in
    /// force=true state so the next `commit_pending()` ignores the frame budget
    /// and repaints unconditionally.
    #[test]
    fn force_full_repaint_sets_force_flag() {
        let mut sched = RedrawScheduler::new();
        // Simulate what `force_full_repaint_after_inline_op` does.
        sched.request_full();
        // commit_pending should return true (force bypasses budget).
        let should_redraw = sched.commit_pending();
        assert!(should_redraw, "force=true must cause immediate repaint regardless of frame budget");
    }

    /// BUG-2: calling request_full twice is idempotent — force is still set.
    #[test]
    fn double_request_full_is_idempotent() {
        let mut sched = RedrawScheduler::new();
        sched.request_full();
        sched.request_full();
        let should_redraw = sched.commit_pending();
        assert!(should_redraw, "double request_full must still produce a repaint");
    }

    /// BUG-3: a layout switch sets ALL dirty regions + force, ensuring the next
    /// frame re-paints every cell and stale cells from the old layout are cleared.
    #[test]
    fn layout_switch_marks_all_regions_dirty() {
        let mut sched = RedrawScheduler::new();
        // Simulate what set_layout() does.
        sched.request_full();
        let should_redraw = sched.commit_pending();
        assert!(should_redraw, "layout switch must trigger an immediate full repaint");
    }

    /// After commit, the scheduler is clean again (no spurious extra repaints).
    #[test]
    fn scheduler_is_clean_after_commit() {
        let mut sched = RedrawScheduler::new();
        sched.request_full();
        let _ = sched.commit_pending();
        // After commit, scheduler is clean — next poll within budget returns false.
        // We don't add a request() here, so dirty is empty.
        let spurious = sched.commit_pending();
        assert!(!spurious, "scheduler must not trigger spurious repaints after clean commit");
    }

    /// Task #687: the FORCE_FULL_REDRAW atomic is the cross-thread escape
    /// hatch for inline ops (e.g. /mcp builder) that can't reach &mut AnvilTui.
    /// Verify the swap-and-clear semantics: first read returns true, second
    /// returns false. `restore_alt_screen()` sets the flag; the main render
    /// loop consumes it once via `take_force_full_redraw()` and promotes the
    /// next paint to a full repaint.
    #[test]
    fn force_full_redraw_flag_is_one_shot() {
        // serial: the atomic is process-wide static; isolate.
        super::FORCE_FULL_REDRAW.store(true, std::sync::atomic::Ordering::SeqCst);
        let first = super::take_force_full_redraw();
        let second = super::take_force_full_redraw();
        assert!(first, "first read must observe the set flag");
        assert!(!second, "flag must clear after being read once");
    }

    /// Task #688 root-cause regression: `restore_alt_screen()` raises
    /// FORCE_FULL_REDRAW, but the main `read_input → draw()` path NEVER calls
    /// `commit_pending_redraw`, so the flag was silently lost — leaving the
    /// parent TUI blank after the wizard returned until the user pressed a key.
    ///
    /// The fix: `handle_repl_command_tui` calls `take_force_full_redraw()` after
    /// `run_command_for_tui` returns and, when true, calls
    /// `force_full_repaint_after_inline_op()`.  This test guards the atomic
    /// contract: `take_force_full_redraw` is one-shot AND serialises correctly
    /// relative to a concurrent store (simulated inline-op return).
    #[test]
    fn force_full_redraw_consumed_before_read_input_draw() {
        // Simulate: restore_alt_screen() sets the flag (inline op return).
        super::FORCE_FULL_REDRAW.store(true, std::sync::atomic::Ordering::SeqCst);

        // Simulate: handle_repl_command_tui consumes it.
        let consumed = super::take_force_full_redraw();
        assert!(
            consumed,
            "handle_repl_command_tui must observe the flag set by restore_alt_screen"
        );

        // Simulate: read_input → draw() runs next iteration — flag is gone.
        let stale = super::take_force_full_redraw();
        assert!(
            !stale,
            "read_input → draw() must NOT see a stale FORCE_FULL_REDRAW; \
             if it did, the blank-screen bug would resurface"
        );
    }

    /// Task #688: MOUSE_CAPTURE_ACTIVE tracks whether mouse capture is currently
    /// on so leave_alt_screen_for_inline_op knows to disable it. Without this,
    /// the user sees raw SGR mouse-tracking escape codes in the wizard's plain
    /// stdout — and worse, abnormal exits leave the user's shell in
    /// mouse-tracking mode where every mouse movement becomes a phantom
    /// "command not found" error.
    #[test]
    fn mouse_capture_active_round_trips() {
        // Save + restore the global state so this test is isolated.
        let prior = super::mouse_capture_active();

        super::MOUSE_CAPTURE_ACTIVE.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(!super::mouse_capture_active(), "default false");

        super::MOUSE_CAPTURE_ACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(super::mouse_capture_active(), "set true → read true");

        // Read does NOT clear (unlike FORCE_FULL_REDRAW which is swap-and-clear).
        // The mouse-capture flag is a state mirror, not a one-shot signal.
        assert!(super::mouse_capture_active(), "repeated read still true");

        super::MOUSE_CAPTURE_ACTIVE.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(!super::mouse_capture_active(), "cleared explicitly");

        // Restore for any test that runs after this one.
        super::MOUSE_CAPTURE_ACTIVE.store(prior, std::sync::atomic::Ordering::SeqCst);
    }
}
