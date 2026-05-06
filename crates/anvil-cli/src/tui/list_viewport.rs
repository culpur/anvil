//! Pure-logic scroll viewport for vertical lists.
//!
//! Used by overflowing dialogs (configure-menu screens, completion popup) so
//! that contents which exceed the available rows can be scrolled instead of
//! being silently truncated.  The math here is intentionally terminal-free —
//! every public function takes the relevant counts as arguments and returns a
//! new state, so it can be exercised without standing up a real `Frame`.
//!
//! The contract mirrors `scrollback::ScrollbackState` in spirit:
//!   * `offset` is the index of the line currently rendered at the top of
//!     the viewport (0 = first item, `total - height` clamps the bottom).
//!   * Movement helpers always clamp to `[0, max_offset]` and never panic
//!     on degenerate inputs (empty list, zero-height viewport).
//!   * Selection-follow is opt-in via [`ListViewport::follow_selection`]:
//!     the offset is only adjusted when the selected row would otherwise
//!     fall outside the visible window.
//!
//! Used keys (wired in `input_handler.rs` / `mod.rs`):
//!   * `Up`/`Down`           → 1-line nudge
//!   * `PageUp`/`PageDown`   → viewport-height jump
//!   * `Home`/`End`          → top / bottom
//!   * `MouseEventKind::ScrollUp/ScrollDown` → 3-line nudge

/// Vertical viewport state for a list of `total` items rendered in a window
/// of `height` rows.  Owns nothing but a single `usize` offset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ListViewport {
    /// Index of the line currently shown at the top of the viewport.
    /// 0 = top of the list; `max_offset(total, height)` = scrolled to bottom.
    offset: usize,
}

impl ListViewport {
    /// Maximum legal offset for the given list/viewport sizes.
    /// Returns 0 when the list fits entirely in the viewport.
    #[must_use]
    pub(crate) const fn max_offset(total: usize, height: usize) -> usize {
        if total > height { total - height } else { 0 }
    }

    /// Construct a viewport pinned at the top of the list.
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self { offset: 0 }
    }

    /// Current top-of-viewport offset (clamped against `total` and `height`).
    #[must_use]
    pub(crate) fn offset(self, total: usize, height: usize) -> usize {
        self.offset.min(Self::max_offset(total, height))
    }

    /// Number of items visible given the current `total` and `height`.
    /// Used by tests; in render code we use `height` directly because we
    /// already pad-or-truncate.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn visible_count(self, total: usize, height: usize) -> usize {
        let off = self.offset(total, height);
        height.min(total.saturating_sub(off))
    }

    /// Scroll up by `n` rows (toward the top of the list).
    #[must_use]
    pub(crate) const fn scroll_up(self, n: usize) -> Self {
        Self { offset: self.offset.saturating_sub(n) }
    }

    /// Scroll down by `n` rows (toward the bottom).  Clamps to `max_offset`.
    #[must_use]
    pub(crate) fn scroll_down(self, n: usize, total: usize, height: usize) -> Self {
        let max = Self::max_offset(total, height);
        Self { offset: self.offset.saturating_add(n).min(max) }
    }

    /// Page up by one viewport height.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) const fn page_up(self, height: usize) -> Self {
        Self { offset: self.offset.saturating_sub(height) }
    }

    /// Page down by one viewport height.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn page_down(self, total: usize, height: usize) -> Self {
        self.scroll_down(height, total, height)
    }

    /// Jump to the top of the list.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) const fn home(self) -> Self {
        Self { offset: 0 }
    }

    /// Jump to the bottom of the list (so that the last row is visible).
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn end(self, total: usize, height: usize) -> Self {
        Self { offset: Self::max_offset(total, height) }
    }

    /// Adjust the offset so that `selected` is on-screen.  Idempotent when
    /// the selection is already visible; otherwise scrolls the minimum
    /// distance needed to reveal the row.
    ///
    /// This is what makes Up/Down on the configure menu feel right when the
    /// menu is taller than the viewport: the user moves selection, and the
    /// viewport follows.
    #[must_use]
    pub(crate) fn follow_selection(
        self,
        selected: usize,
        total: usize,
        height: usize,
    ) -> Self {
        if height == 0 || total == 0 {
            return Self { offset: 0 };
        }
        let max = Self::max_offset(total, height);
        let mut off = self.offset.min(max);
        if selected < off {
            off = selected;
        } else if selected >= off + height {
            off = selected + 1 - height;
        }
        Self { offset: off.min(max) }
    }

    /// Reset the viewport to the top.  Convenient when the list contents
    /// change underneath us (e.g. switching configure screens).
    pub(crate) fn reset(&mut self) {
        self.offset = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::ListViewport;

    // ── max_offset / fitting list ────────────────────────────────────────────

    #[test]
    fn max_offset_zero_when_list_fits() {
        assert_eq!(ListViewport::max_offset(5, 10), 0);
        assert_eq!(ListViewport::max_offset(10, 10), 0);
    }

    #[test]
    fn max_offset_clamps_to_difference() {
        assert_eq!(ListViewport::max_offset(100, 10), 90);
        assert_eq!(ListViewport::max_offset(11, 10), 1);
    }

    #[test]
    fn max_offset_handles_zero_height() {
        // Degenerate viewport — math must not panic.
        assert_eq!(ListViewport::max_offset(50, 0), 50);
    }

    // ── scroll_down + scroll_up ──────────────────────────────────────────────

    #[test]
    fn scroll_down_one_at_a_time() {
        // 100 items in a 10-row viewport.  After 5 down-scrolls,
        // the top of the viewport should be line 5.
        let mut v = ListViewport::new();
        for _ in 0..5 {
            v = v.scroll_down(1, 100, 10);
        }
        assert_eq!(v.offset(100, 10), 5);
        assert_eq!(v.visible_count(100, 10), 10);
    }

    #[test]
    fn scroll_down_clamps_at_bottom() {
        let v = ListViewport::new().scroll_down(10_000, 100, 10);
        assert_eq!(v.offset(100, 10), 90);
    }

    #[test]
    fn scroll_up_clamps_at_top() {
        let v = ListViewport::new().scroll_down(50, 100, 10).scroll_up(1000);
        assert_eq!(v.offset(100, 10), 0);
    }

    // ── page_up / page_down ──────────────────────────────────────────────────

    /// Spec from the task: 100 items in a 10-row viewport, three PgDn presses
    /// should land the viewport showing items 30-39 (offset == 30).
    #[test]
    fn pgdn_three_times_shows_30_to_39() {
        let mut v = ListViewport::new();
        for _ in 0..3 {
            v = v.page_down(100, 10);
        }
        let off = v.offset(100, 10);
        assert_eq!(off, 30, "PgDn x3 should land on item 30");
        // The visible window is items [30, 40) — i.e. 30..=39 inclusive.
        assert_eq!(off + v.visible_count(100, 10), 40);
    }

    #[test]
    fn pgup_after_pgdn_is_inverse() {
        let v = ListViewport::new().page_down(100, 10).page_down(100, 10);
        assert_eq!(v.offset(100, 10), 20);
        let v = v.page_up(10).page_up(10);
        assert_eq!(v.offset(100, 10), 0);
    }

    #[test]
    fn pgdn_clamps_at_bottom_of_long_list() {
        // 25 items, height 10 → max_offset 15.  Three PgDn should clamp to 15.
        let v = ListViewport::new()
            .page_down(25, 10)
            .page_down(25, 10)
            .page_down(25, 10);
        assert_eq!(v.offset(25, 10), 15);
    }

    // ── home / end ───────────────────────────────────────────────────────────

    #[test]
    fn home_returns_to_zero() {
        let v = ListViewport::new().scroll_down(50, 100, 10);
        assert_eq!(v.home().offset(100, 10), 0);
    }

    #[test]
    fn end_lands_at_max_offset() {
        let v = ListViewport::new().end(100, 10);
        assert_eq!(v.offset(100, 10), 90);
    }

    #[test]
    fn end_zero_when_list_fits() {
        let v = ListViewport::new().end(5, 10);
        assert_eq!(v.offset(5, 10), 0);
    }

    // ── follow_selection ─────────────────────────────────────────────────────

    #[test]
    fn follow_selection_idempotent_when_visible() {
        // Selection 5 is visible in offset=0..10.  Should not move.
        let v = ListViewport::new().follow_selection(5, 100, 10);
        assert_eq!(v.offset(100, 10), 0);
    }

    #[test]
    fn follow_selection_scrolls_down_when_below_viewport() {
        // Offset 0, viewport 10, select item 12 → must reveal row 12.
        // Minimum offset that contains row 12 in a height-10 viewport is 3.
        let v = ListViewport::new().follow_selection(12, 100, 10);
        assert_eq!(v.offset(100, 10), 3);
    }

    #[test]
    fn follow_selection_scrolls_up_when_above_viewport() {
        // Start at offset 50, then jump selection to row 4.  Offset must drop.
        let v = ListViewport::new()
            .scroll_down(50, 100, 10)
            .follow_selection(4, 100, 10);
        assert_eq!(v.offset(100, 10), 4);
    }

    #[test]
    fn follow_selection_handles_empty_list() {
        let v = ListViewport::new().follow_selection(0, 0, 10);
        assert_eq!(v.offset(0, 10), 0);
    }

    #[test]
    fn follow_selection_handles_zero_height() {
        let v = ListViewport::new().follow_selection(5, 100, 0);
        assert_eq!(v.offset(100, 0), 0);
    }

    #[test]
    fn reset_clears_offset() {
        let mut v = ListViewport::new().scroll_down(40, 100, 10);
        assert_ne!(v.offset(100, 10), 0);
        v.reset();
        assert_eq!(v.offset(100, 10), 0);
    }

    // ── Combined navigation: simulates the configure-menu flow ───────────────

    #[test]
    fn pgdn_then_arrow_selection_scenario() {
        // Realistic configure flow: 36-widget WidgetPicker in an 8-row viewport.
        // PgDn once → offset 8.  Arrow down still drives selection, follow
        // keeps the highlighted row in view.
        let v = ListViewport::new().page_down(36, 8);
        assert_eq!(v.offset(36, 8), 8);

        // User selects row 7 (above current viewport) → viewport scrolls up.
        let v = v.follow_selection(7, 36, 8);
        assert_eq!(v.offset(36, 8), 7);

        // Now End — last row visible.
        let v = v.end(36, 8);
        assert_eq!(v.offset(36, 8), 28);
        // Verify final visible window is rows 28..=35.
        let last_visible = v.offset(36, 8) + v.visible_count(36, 8);
        assert_eq!(last_visible, 36);
    }
}
