/// v2.2.16 TUI Layout system — module root.
///
/// File layout:
///   mod.rs            — `TuiLayoutRenderer` trait, `LayoutLocalState`, `dispatch_render`
///   vertical_split.rs — A/D renderer  (existing drawing code, extracted)
///   three_pane.rs     — B/E renderer
///   journal.rs        — C/F renderer
///   common.rs         — shared sub-renderers (tab strip, model bar, completion popup)
///
/// The existing `tui/layout.rs` file is untouched; it owns status-line
/// span-builders and cursor math. Our new module is the *plural* `layouts/`
/// — distinct enough to avoid import confusion.

pub(super) mod common;
pub(super) mod journal;
pub(super) mod three_pane;
pub(super) mod vertical_split;

use ratatui::Frame;
use runtime::TuiLayoutConfig;
use runtime::TuiLayoutKind;

use super::snapshot::LayoutSnapshot;

// ─── LayoutLocalState ─────────────────────────────────────────────────────────

/// Layout-local visual state — fields that DON'T survive a live layout switch.
///
/// All shared conversation state (`Tab.log`, `Tab.input`, `Tab.cursor`, etc.)
/// lives on `AnvilTui` / `Tab` and is NOT touched by `set_layout()`. Only the
/// rendering-specific modal/mode fields are reset here.
///
/// Per spec §4 locked decisions:
/// - `VerticalSplit`: right deck cycles Conversation → Transcript → ToolResults
/// - `ThreePane`: vim Normal/Insert/Command modal; Esc from Insert DISCARDS draft
/// - `Journal`: Ctrl-K palette open/query/selection
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LayoutLocalState {
    VerticalSplit {
        /// Which content view is displayed in the right deck.
        right_deck_mode: RightDeckMode,
        /// Index of the selected session in the left rail (future use).
        rail_selected: usize,
    },
    ThreePane {
        /// Current vim modal mode.
        vim_mode: VimMode,
        /// The `:` command line content (e.g. ":q", ":w", ":bd").
        command_line: String,
    },
    Journal {
        /// Whether the Ctrl-K command palette is open.
        palette_open: bool,
        /// Current fuzzy-search query string in the palette.
        palette_query: String,
        /// Currently highlighted row in the palette results.
        palette_selected: usize,
    },
}

/// Right-deck display mode for Layout A/D.
///
/// Cycled by Ctrl+R. `Conversation` is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum RightDeckMode {
    /// Show the conversation log (chat history + streaming text). Default.
    #[default]
    Conversation,
    /// Show the transcript (all log entries in verbose/expanded mode).
    Transcript,
    /// Show only tool calls and their results.
    ToolResults,
}

impl RightDeckMode {
    /// Cycle to the next mode (Ctrl+R).
    pub(super) fn next(self) -> Self {
        match self {
            Self::Conversation => Self::Transcript,
            Self::Transcript => Self::ToolResults,
            Self::ToolResults => Self::Conversation,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Conversation => "Conversation",
            Self::Transcript => "Transcript",
            Self::ToolResults => "Tool Results",
        }
    }
}

/// Vim modal mode for Layout B/E.
///
/// Per spec §11 locked decision #4:
/// - `Esc` from Insert DISCARDS the draft (true vim semantics).
/// - `i` enters Insert with a blank buffer.
/// - `:w` in Command saves the draft to `Tab.input` without submitting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) enum VimMode {
    /// NORMAL — j/k scroll LOG, gt/gT tab, `i` to enter input.
    #[default]
    Normal,
    /// INSERT — cursor active in input row; Enter submits; Esc discards draft.
    Insert,
    /// COMMAND — ex-command line at bottom; `:q` exit, `:w` save, `:bd` close.
    Command,
}

impl LayoutLocalState {
    /// Construct the default local state for a given layout kind.
    /// Called during `AnvilTui::set_layout()` to reset visual-only state.
    pub(super) fn for_kind(kind: TuiLayoutKind) -> Self {
        match kind {
            TuiLayoutKind::VerticalSplit => Self::VerticalSplit {
                right_deck_mode: RightDeckMode::Conversation,
                rail_selected: 0,
            },
            TuiLayoutKind::ThreePane => Self::ThreePane {
                vim_mode: VimMode::Normal,
                command_line: String::new(),
            },
            TuiLayoutKind::Journal => Self::Journal {
                palette_open: false,
                palette_query: String::new(),
                palette_selected: 0,
            },
        }
    }
}

// ─── TuiLayoutRenderer trait ──────────────────────────────────────────────────

/// One implementation per architectural family (A/D, B/E, C/F).
///
/// Each impl handles BOTH variants (with/without tabs) by branching on
/// `self.tabs` internally — this keeps the dispatch table small and the
/// variant-specific code co-located.
///
/// Per spec §6.
pub(super) trait TuiLayoutRenderer {
    /// Paint one frame. Pure function of inputs; mutates only `local` for
    /// cursor / scrollback follow. Tab-hit geometry is written into
    /// `tab_hits_out` so the input handler can dispatch mouse clicks.
    fn render(
        &self,
        frame: &mut Frame,
        snap: &LayoutSnapshot,
        local: &mut LayoutLocalState,
        tab_hits_out: &mut Vec<crate::tui::TabHit>,
    );
}

// ─── Dispatcher ───────────────────────────────────────────────────────────────

/// Route a frame to the correct layout renderer based on `cfg`.
///
/// Called from `AnvilTui::draw()` in place of the former inline rendering block.
/// The `collect_snapshot()` call remains above this in `draw()`. Modals
/// (configure overlay, permission, SSH form, screensaver) are rendered by the
/// caller AFTER this returns so they appear on top of any layout.
pub(super) fn dispatch_render(
    cfg: TuiLayoutConfig,
    frame: &mut Frame,
    snap: &LayoutSnapshot,
    local: &mut LayoutLocalState,
    tab_hits_out: &mut Vec<crate::tui::TabHit>,
) {
    match cfg.kind {
        TuiLayoutKind::VerticalSplit => {
            vertical_split::Renderer { tabs: cfg.tabs }.render(frame, snap, local, tab_hits_out)
        }
        TuiLayoutKind::ThreePane => {
            three_pane::Renderer { tabs: cfg.tabs }.render(frame, snap, local, tab_hits_out)
        }
        TuiLayoutKind::Journal => {
            journal::Renderer { tabs: cfg.tabs }.render(frame, snap, local, tab_hits_out)
        }
    }
}
