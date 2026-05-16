// tui/redraw.rs — unified redraw scheduler
//
// Background: across v2.1.x and v2.2.x we accumulated several distinct redraw
// bugs (BUG-122-8 idle-draw guard regression, BUG-129-4 agent panel regression,
// /model list keypress requirement, model-switch println bleed). All of them
// shared the same shape — a `needs_redraw` boolean (or its absence) was set in
// one place and missed in another. Each fix targeted the single bug; none
// addressed the underlying class.
//
// This module centralizes the decision "should we paint this frame?" behind a
// dirty-region bitset and a frame-budget coalescer. Call sites that produce a
// visible change call `request(region)`; the main loop calls `commit()` once
// per iteration. `commit()` short-circuits when nothing is dirty (saves the
// tmux -CC flood from #315) and forces a full repaint when explicitly asked
// (modal close, model switch, tab switch).
//
// Design notes:
//   - DirtyRegions is a bitset, NOT a single bool, so we can layer in
//     region-specific renderers later (input-only repaint, status-line-only)
//     without changing the API.
//   - `request_full()` is a hard reset: it forces the next commit to repaint
//     everything regardless of frame budget. Use it after structural changes
//     (tab/model switch, modal close, sandbox-mode change) where any
//     short-circuit would leave stale state on screen.
//   - The frame budget defaults to 60 fps (16.67ms). When commits arrive
//     faster than the budget, we mark dirty but don't repaint until the
//     budget elapses. This prevents the BUG-315 tmux idle-flood class.
//   - For now, `commit()` always calls the underlying `draw()` (i.e. full
//     repaint) when ANY region is dirty — region-targeted partial repaints
//     are a future optimization that requires splitting `draw()` into
//     per-region renderers. The structural separation is in place; the cost
//     model is conservative until partial renderers are wired.

use std::time::{Duration, Instant};

/// Bitset of regions that have changed and need repainting.
///
/// v2.2.16: Widened from `u8` to `u16` to accommodate new layout-specific
/// regions (RAIL, FOCUS, CONTEXT). Existing callers (`request(DirtyRegions::INPUT)`
/// etc.) are API-stable — no changes required at call sites.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirtyRegions(u16);

impl DirtyRegions {
    /// Input box (current prompt + cursor + composing draft).
    pub const INPUT: Self = Self(1 << 0);
    /// Status line (model, provider, tokens, git, effort).
    pub const STATUS: Self = Self(1 << 1);
    /// Main scrollback / chat log.
    pub const SCROLLBACK: Self = Self(1 << 2);
    /// Agent panel (subagent tree, task list).
    pub const AGENT_PANEL: Self = Self(1 << 3);
    /// Tab strip across the top (also reused as buffer-line / thread-switcher region).
    pub const TAB_STRIP: Self = Self(1 << 4);
    /// Configure overlay (when active).
    pub const OVERLAY: Self = Self(1 << 5);
    /// Anything not covered above (think spinner, completion popup).
    pub const MISC: Self = Self(1 << 6);

    // ── v2.2.16 Layout-specific regions ────────────────────────────────────
    /// Layout A/D left rail (session list). Scoped repaint target.
    pub const RAIL: Self = Self(1 << 7);
    /// Layout B/E FOCUS pane (current assistant action). Scoped repaint target.
    pub const FOCUS: Self = Self(1 << 8);
    /// Layout B/E CONTEXT strip (model/tokens/cost/git). Scoped repaint target.
    pub const CONTEXT: Self = Self(1 << 9);
    // Note: JOURNAL_BODY reuses SCROLLBACK (single-column → same region).
    // BUFFER_LINE / THREAD_SWITCHER reuse TAB_STRIP (same role).

    /// All regions (updated for v2.2.16 with the three new bits).
    pub const ALL: Self = Self(0b0000_0011_1111_1111);

    /// No regions dirty.
    pub const NONE: Self = Self(0);

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl std::ops::BitOr for DirtyRegions {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for DirtyRegions {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Coordinates redraws so call sites don't have to think about timing.
///
/// State machine:
///   - `request(region)` ORs the region into the dirty set
///   - `request_full()` ORs `ALL` and sets a force flag (next `commit()` paints
///     even if we're inside the frame budget)
///   - `commit_pending()` returns true iff the caller should call the
///     underlying `draw()` AND clears the dirty set
///
/// Frame budget: by default we cap at ~60 fps (16ms). `commit_pending()`
/// returns false if we're inside the budget unless the force flag is set.
#[derive(Debug)]
pub struct RedrawScheduler {
    dirty: DirtyRegions,
    last_commit: Option<Instant>,
    frame_budget: Duration,
    force_next: bool,
}

impl Default for RedrawScheduler {
    fn default() -> Self {
        Self {
            dirty: DirtyRegions::NONE,
            last_commit: None,
            // ~60 fps. Tested: lower than this saves CPU during streaming with
            // imperceptible UX impact; faster than this floods tmux -CC.
            frame_budget: Duration::from_millis(16),
            force_next: false,
        }
    }
}

impl RedrawScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark one or more regions dirty. Cheap; no I/O.
    pub fn request(&mut self, region: DirtyRegions) {
        self.dirty |= region;
    }

    /// Mark every region dirty AND force the next commit to paint regardless of
    /// frame budget. Use after tab switch, model switch, modal close, or any
    /// structural change where short-circuiting would leave stale pixels.
    pub fn request_full(&mut self) {
        self.dirty = DirtyRegions::ALL;
        self.force_next = true;
    }

    /// Returns true iff the caller should perform a draw NOW. Clears the dirty
    /// set on a positive answer; leaves it on a negative answer (we'll try
    /// again next time).
    pub fn commit_pending(&mut self) -> bool {
        if self.dirty.is_empty() && !self.force_next {
            return false;
        }
        if !self.force_next {
            if let Some(last) = self.last_commit {
                if last.elapsed() < self.frame_budget {
                    return false;
                }
            }
        }
        self.dirty = DirtyRegions::NONE;
        self.force_next = false;
        self.last_commit = Some(Instant::now());
        true
    }

    /// Returns the current dirty set without clearing it. For tests.
    #[allow(dead_code)]
    pub fn peek_dirty(&self) -> DirtyRegions {
        self.dirty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_scheduler_has_no_pending() {
        let mut s = RedrawScheduler::new();
        assert!(!s.commit_pending());
    }

    #[test]
    fn request_then_commit_returns_true_once() {
        let mut s = RedrawScheduler::new();
        s.request(DirtyRegions::INPUT);
        assert!(s.commit_pending(), "first commit after request should fire");
        // Within frame budget, even if dirty, should NOT fire again
        s.request(DirtyRegions::INPUT);
        assert!(!s.commit_pending(), "second commit within frame budget should be deferred");
    }

    #[test]
    fn request_full_bypasses_frame_budget() {
        let mut s = RedrawScheduler::new();
        s.request(DirtyRegions::INPUT);
        assert!(s.commit_pending());
        // Immediately ask for full — should fire despite budget
        s.request_full();
        assert!(s.commit_pending(), "request_full must bypass frame budget");
    }

    #[test]
    fn empty_request_does_not_fire() {
        let mut s = RedrawScheduler::new();
        // No request call
        assert!(!s.commit_pending(), "empty request must not fire (saves tmux flood)");
    }

    #[test]
    fn dirty_persists_when_budget_blocks() {
        let mut s = RedrawScheduler::new();
        s.request(DirtyRegions::STATUS);
        assert!(s.commit_pending());
        s.request(DirtyRegions::INPUT);
        // Within budget — commit deferred
        assert!(!s.commit_pending());
        // Verify the dirty state survives the deferred commit
        assert!(s.peek_dirty().contains(DirtyRegions::INPUT));
    }

    #[test]
    fn dirty_regions_bitset_or_works() {
        let combined = DirtyRegions::INPUT | DirtyRegions::STATUS;
        assert!(combined.contains(DirtyRegions::INPUT));
        assert!(combined.contains(DirtyRegions::STATUS));
        assert!(!combined.contains(DirtyRegions::SCROLLBACK));
    }

    #[test]
    fn dirty_or_assign_accumulates() {
        let mut d = DirtyRegions::INPUT;
        d |= DirtyRegions::STATUS;
        d |= DirtyRegions::SCROLLBACK;
        assert!(d.contains(DirtyRegions::INPUT));
        assert!(d.contains(DirtyRegions::STATUS));
        assert!(d.contains(DirtyRegions::SCROLLBACK));
    }

    #[test]
    fn dirty_all_contains_each_region() {
        let all = DirtyRegions::ALL;
        for region in [
            DirtyRegions::INPUT,
            DirtyRegions::STATUS,
            DirtyRegions::SCROLLBACK,
            DirtyRegions::AGENT_PANEL,
            DirtyRegions::TAB_STRIP,
            DirtyRegions::OVERLAY,
            DirtyRegions::MISC,
            // v2.2.16 layout-specific regions
            DirtyRegions::RAIL,
            DirtyRegions::FOCUS,
            DirtyRegions::CONTEXT,
        ] {
            assert!(all.contains(region));
        }
    }

    #[test]
    fn force_clears_after_commit() {
        let mut s = RedrawScheduler::new();
        s.request_full();
        assert!(s.commit_pending());
        // Force flag should NOT persist — next idle commit is silent
        assert!(!s.commit_pending());
    }
}
