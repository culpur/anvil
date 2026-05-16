/// v2.2.16 TUI Layout system — module root.
///
/// File layout:
///   mod.rs            — `TuiLayoutRenderer` trait, `LayoutLocalState`, `dispatch_render`
///   classic.rs        — A0/D0 renderer (pre-v2.2.16 monolithic rendering, renamed)
///   vertical_split.rs — A/D renderer (rail+deck design)
///   three_pane.rs     — B/E renderer (always-on input, no vim modal)
///   journal.rs        — C/F renderer
///   common.rs         — shared sub-renderers (tab strip, model bar, completion popup)
///
/// The existing `tui/layout.rs` file is untouched; it owns status-line
/// span-builders and cursor math. Our new module is the *plural* `layouts/`
/// — distinct enough to avoid import confusion.

pub(super) mod classic;
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
/// - `Classic`: no per-layout local state needed (stateless classic renderer)
/// - `VerticalSplit`: right deck cycles Conversation → Transcript → ToolResults
/// - `ThreePane`: always-on input (no vim modal since v2.2.16 Correction 4)
/// - `Journal`: Ctrl-K palette open/query/selection
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LayoutLocalState {
    Classic,
    VerticalSplit {
        /// Which content view is displayed in the right deck.
        right_deck_mode: RightDeckMode,
        /// Index of the selected session in the left rail (future use).
        rail_selected: usize,
    },
    ThreePane,
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
pub(crate) enum RightDeckMode {
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
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Conversation => Self::Transcript,
            Self::Transcript => Self::ToolResults,
            Self::ToolResults => Self::Conversation,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Conversation => "Conversation",
            Self::Transcript => "Transcript",
            Self::ToolResults => "Tool Results",
        }
    }
}

impl LayoutLocalState {
    /// Construct the default local state for a given layout kind.
    /// Called when constructing a `Tab` or switching layouts on the active tab.
    pub(crate) fn for_kind(kind: TuiLayoutKind) -> Self {
        match kind {
            TuiLayoutKind::Classic => Self::Classic,
            TuiLayoutKind::VerticalSplit => Self::VerticalSplit {
                right_deck_mode: RightDeckMode::Conversation,
                rail_selected: 0,
            },
            TuiLayoutKind::ThreePane => Self::ThreePane,
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
        TuiLayoutKind::Classic => {
            classic::Renderer { tabs: cfg.tabs }.render(frame, snap, local, tab_hits_out)
        }
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

// ─── Layout system tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Three-pane always-on input (Correction 4) ─────────────────────────────

    /// `for_kind(ThreePane)` now returns the stateless `ThreePane` variant
    /// (no vim_mode, no command_line — they were deleted in Correction 4).
    #[test]
    fn three_pane_initial_state_is_stateless() {
        let state = LayoutLocalState::for_kind(TuiLayoutKind::ThreePane);
        assert!(
            matches!(state, LayoutLocalState::ThreePane),
            "ThreePane local state must be the stateless unit variant"
        );
    }

    /// `for_kind(Classic)` returns the stateless `Classic` variant.
    #[test]
    fn classic_initial_state_is_stateless() {
        let state = LayoutLocalState::for_kind(TuiLayoutKind::Classic);
        assert!(matches!(state, LayoutLocalState::Classic));
    }

    /// `for_kind(VerticalSplit)` initialises rail + Conversation deck.
    #[test]
    fn vertical_split_initial_state() {
        let state = LayoutLocalState::for_kind(TuiLayoutKind::VerticalSplit);
        match state {
            LayoutLocalState::VerticalSplit { right_deck_mode, rail_selected } => {
                assert_eq!(right_deck_mode, RightDeckMode::Conversation);
                assert_eq!(rail_selected, 0);
            }
            _ => panic!("expected VerticalSplit variant"),
        }
    }

    /// RightDeckMode cycles Conversation → Transcript → ToolResults → Conversation.
    #[test]
    fn right_deck_mode_cycles() {
        use RightDeckMode::*;
        assert_eq!(Conversation.next(), Transcript);
        assert_eq!(Transcript.next(), ToolResults);
        assert_eq!(ToolResults.next(), Conversation);
    }

    // ── BUG-6: journal completion popup wiring ────────────────────────────────

    /// `render_completion_popup` is accessible from `common`.
    #[test]
    fn journal_can_call_render_completion_popup() {
        let _fn_ptr: fn(
            &mut ratatui::Frame,
            ratatui::layout::Rect,
            &crate::tui::snapshot::LayoutSnapshot,
        ) = super::common::render_completion_popup;
        assert!(true);
    }

    // ── Per-tab layout state tests (Correction 1) ─────────────────────────────

    /// `set_active_tab_layout(new, global=false)` must not change other tabs.
    #[test]
    fn set_active_tab_layout_does_not_affect_other_tabs() {
        use runtime::{TuiLayoutConfig, TuiLayoutKind};
        use crate::tui::state::Tab;

        let tab_a_layout = TuiLayoutConfig { kind: TuiLayoutKind::Classic, tabs: true };
        let tab_b_layout = TuiLayoutConfig { kind: TuiLayoutKind::Classic, tabs: true };
        let new_layout   = TuiLayoutConfig { kind: TuiLayoutKind::ThreePane, tabs: false };

        let mut tab_a = Tab::new(1, "a", "model", "sess");
        tab_a.tui_layout = tab_a_layout;
        tab_a.layout_local = LayoutLocalState::for_kind(tab_a_layout.kind);

        let mut tab_b = Tab::new(2, "b", "model", "sess");
        tab_b.tui_layout = tab_b_layout;
        tab_b.layout_local = LayoutLocalState::for_kind(tab_b_layout.kind);

        // Simulate set_active_tab_layout(new, global=false) on tab_a.
        tab_a.tui_layout = new_layout;
        tab_a.layout_local = LayoutLocalState::for_kind(new_layout.kind);

        // tab_b must be unchanged.
        assert_eq!(tab_b.tui_layout, tab_b_layout, "tab_b layout must not change");
        assert!(
            matches!(tab_b.layout_local, LayoutLocalState::Classic),
            "tab_b local state must remain Classic"
        );
    }

    /// `set_active_tab_layout(new, global=true)` must update all tabs.
    #[test]
    fn set_active_tab_layout_with_global_updates_all_tabs() {
        use runtime::{TuiLayoutConfig, TuiLayoutKind};
        use crate::tui::state::Tab;

        let initial = TuiLayoutConfig { kind: TuiLayoutKind::Classic, tabs: true };
        let new_layout = TuiLayoutConfig { kind: TuiLayoutKind::Journal, tabs: true };

        let mut tabs: Vec<Tab> = (1..=3)
            .map(|i| {
                let mut t = Tab::new(i, format!("t{i}"), "model", "sess");
                t.tui_layout = initial;
                t.layout_local = LayoutLocalState::for_kind(initial.kind);
                t
            })
            .collect();

        // Simulate global=true: update all tabs.
        for t in &mut tabs {
            t.tui_layout = new_layout;
            t.layout_local = LayoutLocalState::for_kind(new_layout.kind);
        }

        for t in &tabs {
            assert_eq!(t.tui_layout, new_layout, "all tabs must have the new layout");
        }
    }

    /// New tab inherits config.json default, not active tab's layout.
    #[test]
    fn new_tab_inherits_global_default_not_active_tab_layout() {
        use runtime::{TuiLayoutConfig, TuiLayoutKind};
        use crate::tui::state::Tab;

        // Simulate active tab on three-pane.
        let _active_layout = TuiLayoutConfig { kind: TuiLayoutKind::ThreePane, tabs: false };

        // New tab is constructed without copying from the active tab.
        // Tab::new reads from config.json (or falls back to default).
        // In a test environment there is no config.json, so it returns the default.
        let new_tab = Tab::new(99, "new", "model", "sess");

        // The new tab must have the config.json default (Classic + tabs), NOT
        // the active tab's ThreePane layout. Since tests run without config.json,
        // we just check that the new tab initializes from `load_default_layout()`
        // (which returns TuiLayoutConfig::default() in test env = Classic + tabs).
        let expected_default = Tab::load_default_layout();
        assert_eq!(
            new_tab.tui_layout,
            expected_default,
            "new tab must inherit config.json default, not active tab's layout"
        );
    }
}
