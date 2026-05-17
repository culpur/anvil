/// Per-tab scrollback ring buffer.
///
/// Stores the last `capacity` rendered text lines so the user can page
/// back through history with PageUp/PageDown regardless of whether the
/// terminal emulator preserves a scrollback region (it won't, because we
/// use the alternate screen buffer).
///
/// Design notes:
/// - Lines are plain `String`s — ANSI codes stripped, matching what the
///   content area renders via `LogEntry::to_lines`.
/// - The buffer is a `VecDeque` used as a ring: when full, the oldest
///   entry is popped from the front before pushing to the back.
/// - `scrollback_pos` in `Tab` is an `Option<usize>`:
///     `None`     → live view (tracking the bottom)
///     `Some(n)`  → viewing line `n` from the end of the buffer
///                  (0 = most-recent, capacity-1 = oldest)

use std::collections::VecDeque;

/// Default maximum number of lines to retain.
pub const DEFAULT_CAPACITY: usize = 10_000;

/// A bounded ring buffer of text lines for in-TUI scrollback.
#[derive(Debug, Clone)]
pub struct ScrollbackBuffer {
    lines: VecDeque<String>,
    capacity: usize,
}

impl ScrollbackBuffer {
    /// Create a new buffer with the given maximum line count.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(capacity.min(1024)),
            capacity,
        }
    }

    /// Create a buffer with [`DEFAULT_CAPACITY`].
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Push a single line.  If the buffer is at capacity the oldest line
    /// is discarded.
    pub fn push(&mut self, line: String) {
        if self.lines.len() >= self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// Pop the last `n` lines from the back of the buffer.
    ///
    /// Used by the per-draw refresh path in `tui::mod::draw` to discard
    /// the trailing "pending text" view before re-pushing the current
    /// pending lines. Without this, an in-flight stream like
    /// `# Anvil-Dev` pushed as several deltas (`#`, ` Anvil`, `-Dev`)
    /// would leave just `#` cached as the first scrollback line forever,
    /// because the grow-only logic never updated already-pushed entries.
    pub fn pop_back_n(&mut self, n: usize) {
        for _ in 0..n.min(self.lines.len()) {
            self.lines.pop_back();
        }
    }

    /// Push many lines at once.
    #[allow(dead_code)]
    pub fn push_lines(&mut self, lines: impl IntoIterator<Item = String>) {
        for line in lines {
            self.push(line);
        }
    }

    /// Total number of lines currently stored.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// True when the buffer contains no lines.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Maximum number of lines this buffer will retain.
    #[allow(dead_code)]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Return a slice of lines suitable for rendering in a viewport of
    /// `height` rows, positioned so that line index `anchor` (0 = oldest)
    /// appears at the top.
    ///
    /// Returns `(lines, anchor_clamped)` so the caller can clamp the
    /// stored anchor after the call.
    #[allow(dead_code)]
    pub fn view(&self, anchor: usize, height: usize) -> (&[String], usize) {
        let total = self.lines.len();
        if total == 0 || height == 0 {
            return (self.as_slice_empty(), 0);
        }
        let max_anchor = total.saturating_sub(height);
        let anchor = anchor.min(max_anchor);

        // VecDeque is contiguous in at most two slices.  Collect into a
        // (logically) flat range via `as_slices` + offset arithmetic.
        let (a, b) = self.lines.as_slices();
        let end = (anchor + height).min(total);
        let range_len = end - anchor;

        // If the desired range falls entirely within the first contiguous
        // slice we can return it directly.
        if anchor + range_len <= a.len() {
            return (&a[anchor..anchor + range_len], anchor);
        }

        // Otherwise we rely on the caller using `lines_in_range` which
        // returns an owned Vec; signal with empty slice and real anchor.
        // (This only matters in tests — the draw path uses `lines_in_range`.)
        let _ = b; // suppress unused warning
        (self.as_slice_empty(), anchor)
    }

    /// Collect `height` lines starting from `anchor` (0 = oldest) into an
    /// owned `Vec`.  Returns `(lines, anchor_clamped)`.
    pub fn lines_in_range(&self, anchor: usize, height: usize) -> (Vec<String>, usize) {
        let total = self.lines.len();
        if total == 0 || height == 0 {
            return (Vec::new(), 0);
        }
        let max_anchor = total.saturating_sub(height);
        let anchor = anchor.min(max_anchor);
        let end = (anchor + height).min(total);
        let lines: Vec<String> = self.lines.range(anchor..end).cloned().collect();
        (lines, anchor)
    }

    /// Compute the anchor that puts the *bottom* of the viewport at the
    /// most-recent line given a viewport `height`.
    pub fn live_anchor(&self, height: usize) -> usize {
        self.lines.len().saturating_sub(height)
    }

    // Internal helper: returns an empty slice with the correct element type.
    #[allow(dead_code)]
    #[inline]
    fn as_slice_empty(&self) -> &[String] {
        &[]
    }
}

impl Default for ScrollbackBuffer {
    fn default() -> Self {
        Self::new()
    }
}

// ─── ScrollbackState ─────────────────────────────────────────────────────────

/// The scrollback navigation state for one tab.
///
/// `None`     → live view (bottom of scrollback, auto-follows new lines)
/// `Some(n)`  → historical view with anchor = n (index into the buffer,
///              0 = oldest line)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbackState(pub Option<usize>);

impl ScrollbackState {
    /// Start in live (bottom) mode.
    pub const fn live() -> Self {
        Self(None)
    }

    /// True when in live view.
    pub fn is_live(self) -> bool {
        self.0.is_none()
    }

    /// True when this state should render AS live for the given buffer +
    /// viewport height. A stored anchor that's at or past the current
    /// live anchor means new content has streamed in to the point where
    /// the user is effectively viewing the live tail — show the live UI,
    /// not the yellow "HISTORICAL VIEW" banner.
    ///
    /// Fixes #591: prior to this helper, `Some(a)` where `a` was a
    /// 1-line mouse-wheel scroll-up would freeze the banner on even
    /// after enough new content arrived that `a >= live_anchor`.
    pub fn is_effectively_live(self, buf: &ScrollbackBuffer, height: usize) -> bool {
        match self.0 {
            None => true,
            Some(a) => a >= buf.live_anchor(height),
        }
    }

    /// Return the anchor to use when rendering.
    ///
    /// `height` is the visible line count of the content area.
    pub fn effective_anchor(self, buf: &ScrollbackBuffer, height: usize) -> usize {
        match self.0 {
            None => buf.live_anchor(height),
            Some(a) => {
                let max = buf.len().saturating_sub(height);
                a.min(max)
            }
        }
    }

    /// Scroll up (toward older content) by `n` lines.
    /// Transitions from live → historical on the first call.
    pub fn page_up(self, buf: &ScrollbackBuffer, height: usize, n: usize) -> Self {
        let current = match self.0 {
            None => buf.live_anchor(height),
            Some(a) => a,
        };
        Self(Some(current.saturating_sub(n)))
    }

    /// Scroll down (toward newer content) by `n` lines.
    /// Automatically returns to live view when it reaches the bottom.
    pub fn page_down(self, buf: &ScrollbackBuffer, height: usize, n: usize) -> Self {
        match self.0 {
            None => Self(None), // already live
            Some(a) => {
                let live = buf.live_anchor(height);
                let new_a = a.saturating_add(n);
                if new_a >= live {
                    Self(None) // snapped back to live
                } else {
                    Self(Some(new_a))
                }
            }
        }
    }

    /// Jump to live view.
    #[allow(dead_code)]
    pub fn go_live(self) -> Self {
        Self(None)
    }
}

/// CC-139-F5 helper: given a sorted ascending list of scrollback line
/// indices that mark user-message starts, find the previous or next one
/// relative to `current_anchor`.
///
/// `forward = false` ⇒ largest entry strictly less than `current_anchor`
/// `forward = true`  ⇒ smallest entry strictly greater than `current_anchor`
///
/// Returns `None` when no qualifying entry exists in that direction; the
/// caller is expected to surface a "No earlier/later user message" system
/// message instead of moving the viewport.
///
/// Extracted as a free function so the navigation logic can be unit-tested
/// without standing up an `AnvilTui`.
pub fn pick_user_anchor(user_lines: &[usize], current_anchor: usize, forward: bool) -> Option<usize> {
    if forward {
        user_lines.iter().copied().find(|&p| p > current_anchor)
    } else {
        user_lines.iter().copied().rev().find(|&p| p < current_anchor)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Ring-buffer bounds ────────────────────────────────────────────────────

    #[test]
    fn buffer_does_not_exceed_capacity() {
        let cap = 50;
        let mut buf = ScrollbackBuffer::with_capacity(cap);
        for i in 0..200 {
            buf.push(format!("line {i}"));
        }
        assert_eq!(buf.len(), cap, "buffer must not grow beyond capacity");
        assert_eq!(
            buf.capacity(),
            cap,
            "reported capacity must match construction arg"
        );
    }

    #[test]
    fn buffer_oldest_lines_are_evicted() {
        let mut buf = ScrollbackBuffer::with_capacity(5);
        for i in 0..10u32 {
            buf.push(format!("L{i}"));
        }
        // Only last 5 should remain: L5..L9
        let (lines, _) = buf.lines_in_range(0, 10);
        assert_eq!(lines, vec!["L5", "L6", "L7", "L8", "L9"]);
    }

    #[test]
    fn buffer_empty_view_is_safe() {
        let buf = ScrollbackBuffer::new();
        let (lines, anchor) = buf.lines_in_range(0, 20);
        assert!(lines.is_empty());
        assert_eq!(anchor, 0);
    }

    #[test]
    fn buffer_partial_fill() {
        let mut buf = ScrollbackBuffer::with_capacity(100);
        buf.push("hello".into());
        buf.push("world".into());
        assert_eq!(buf.len(), 2);
        let (lines, _) = buf.lines_in_range(0, 10);
        assert_eq!(lines, vec!["hello", "world"]);
    }

    #[test]
    fn buffer_push_lines() {
        let mut buf = ScrollbackBuffer::with_capacity(10);
        buf.push_lines(vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn pop_back_n_removes_trailing_lines() {
        let mut buf = ScrollbackBuffer::with_capacity(100);
        buf.push_lines(vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        buf.pop_back_n(2);
        let (lines, _) = buf.lines_in_range(0, 10);
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[test]
    fn pop_back_n_saturates_at_buffer_size() {
        // Asking to pop more than is present must not panic or underflow.
        let mut buf = ScrollbackBuffer::with_capacity(100);
        buf.push("only".into());
        buf.pop_back_n(99);
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn streaming_deltas_replace_not_append() {
        // Reproduces the v2.2.12 bug where a streaming assistant message
        // had its first line truncated in HISTORICAL VIEW. The draw path
        // used to grow scrollback as text arrived, leaving "#" cached as
        // line 1 even after the full "# Anvil-Dev Analysis Summary" had
        // streamed in. The fix tracks how many trailing lines came from
        // pending_text and rewrites them each frame instead of appending.
        let mut buf = ScrollbackBuffer::with_capacity(100);
        let mut pending_count: usize = 0;

        // Frame 1: first delta arrives.
        buf.pop_back_n(pending_count);
        pending_count = 0;
        for line in "#".lines() {
            buf.push(line.to_string());
            pending_count += 1;
        }
        let (lines, _) = buf.lines_in_range(0, 10);
        assert_eq!(lines, vec!["#"]);

        // Frame 2: more text arrived on the same line.
        buf.pop_back_n(pending_count);
        pending_count = 0;
        for line in "# Anvil-Dev Analysis Summary".lines() {
            buf.push(line.to_string());
            pending_count += 1;
        }
        let (lines, _) = buf.lines_in_range(0, 10);
        assert_eq!(
            lines,
            vec!["# Anvil-Dev Analysis Summary"],
            "scrollback must show the full streamed line, not just '#' (was the bug)"
        );
    }

    // ── PageUp / PageDown navigation ──────────────────────────────────────────

    #[test]
    fn page_up_enters_historical_view() {
        let mut buf = ScrollbackBuffer::with_capacity(100);
        for i in 0..50u32 {
            buf.push(format!("L{i}"));
        }
        let state = ScrollbackState::live();
        assert!(state.is_live());

        let state = state.page_up(&buf, 20, 10);
        assert!(
            !state.is_live(),
            "after PageUp should be in historical view"
        );
    }

    #[test]
    fn page_up_clamps_to_zero() {
        let mut buf = ScrollbackBuffer::with_capacity(100);
        for i in 0..5u32 {
            buf.push(format!("L{i}"));
        }
        let state = ScrollbackState::live();
        // Page way up — should clamp to anchor 0
        let state = state.page_up(&buf, 5, 1000);
        assert_eq!(state.0, Some(0));
    }

    #[test]
    fn page_down_returns_to_live_at_bottom() {
        let mut buf = ScrollbackBuffer::with_capacity(100);
        for i in 0..50u32 {
            buf.push(format!("L{i}"));
        }
        let height = 20;
        // Start at top of history
        let state = ScrollbackState(Some(0));
        // Page way down — should snap back to live
        let state = state.page_down(&buf, height, 10_000);
        assert!(
            state.is_live(),
            "paging past the bottom must return to live view"
        );
    }

    #[test]
    fn page_down_noop_when_already_live() {
        let mut buf = ScrollbackBuffer::with_capacity(100);
        buf.push("x".into());
        let state = ScrollbackState::live();
        let state2 = state.page_down(&buf, 20, 5);
        assert!(state2.is_live());
    }

    #[test]
    fn go_live_clears_historical() {
        let mut buf = ScrollbackBuffer::with_capacity(100);
        buf.push("x".into());
        let state = ScrollbackState(Some(0));
        assert!(!state.is_live());
        let state = state.go_live();
        assert!(state.is_live());
    }

    // ── Modifier-shift detection helper ──────────────────────────────────────

    // ── CC-139-F5 transcript user-anchor picker (#460) ───────────────────────

    #[test]
    fn pick_user_anchor_finds_previous_user_message() {
        // Three user messages at scrollback rows 0, 12, 27.
        // Cursor at row 30 — backward jump should land on 27.
        let users = vec![0usize, 12, 27];
        let prev = pick_user_anchor(&users, 30, false);
        assert_eq!(prev, Some(27), "`{{` from row 30 should jump to row 27");
    }

    #[test]
    fn pick_user_anchor_finds_next_user_message() {
        // Cursor at row 5 — forward jump should land on 12.
        let users = vec![0usize, 12, 27];
        let next = pick_user_anchor(&users, 5, true);
        assert_eq!(next, Some(12), "`}}` from row 5 should jump to row 12");
    }

    #[test]
    fn pick_user_anchor_strictly_less_than_current() {
        // Cursor exactly on a user-message row — backward must skip it (no
        // pointless no-op jumps).
        let users = vec![0usize, 12, 27];
        let prev = pick_user_anchor(&users, 12, false);
        assert_eq!(prev, Some(0), "`{{` from row 12 should skip 12 and land on 0");
    }

    #[test]
    fn pick_user_anchor_strictly_greater_than_current() {
        // Cursor exactly on a user-message row — forward must skip it.
        let users = vec![0usize, 12, 27];
        let next = pick_user_anchor(&users, 12, true);
        assert_eq!(next, Some(27), "`}}` from row 12 should skip 12 and land on 27");
    }

    #[test]
    fn pick_user_anchor_returns_none_when_no_earlier_message() {
        // Cursor before the earliest user row — backward returns None so
        // the caller can push "No earlier user message".
        let users = vec![5usize, 18, 33];
        let prev = pick_user_anchor(&users, 4, false);
        assert_eq!(prev, None);
        // Also for cursor at the earliest entry itself.
        let prev2 = pick_user_anchor(&users, 5, false);
        assert_eq!(prev2, None, "exact match must not be returned");
    }

    #[test]
    fn pick_user_anchor_returns_none_when_no_later_message() {
        // Cursor at or past the last user row — forward returns None.
        let users = vec![5usize, 18, 33];
        let next = pick_user_anchor(&users, 33, true);
        assert_eq!(next, None, "exact match must not be returned");
        let next2 = pick_user_anchor(&users, 100, true);
        assert_eq!(next2, None);
    }

    #[test]
    fn pick_user_anchor_handles_empty_input() {
        // No user messages — both directions return None.
        let users: Vec<usize> = Vec::new();
        assert_eq!(pick_user_anchor(&users, 0, false), None);
        assert_eq!(pick_user_anchor(&users, 0, true), None);
    }

    /// Verify that our shift-modifier check is correct for the event kinds
    /// we use.  This is a pure logic test — no terminal required.
    #[test]
    fn shift_drag_detected_correctly() {
        use crossterm::event::{KeyModifiers, MouseEventKind};

        // A drag with SHIFT held → should pass through to terminal
        let kind = MouseEventKind::Drag(crossterm::event::MouseButton::Left);
        let mods = KeyModifiers::SHIFT;
        let is_shift_drag = matches!(kind, MouseEventKind::Drag(_))
            && mods.contains(KeyModifiers::SHIFT);
        assert!(is_shift_drag, "SHIFT + Drag must be identified as pass-through");

        // A scroll wheel with SHIFT → not a drag, should NOT pass through
        let kind2 = MouseEventKind::ScrollUp;
        let is_shift_drag2 = matches!(kind2, MouseEventKind::Drag(_))
            && mods.contains(KeyModifiers::SHIFT);
        assert!(!is_shift_drag2);

        // A drag without SHIFT → Anvil should handle it
        let kind3 = MouseEventKind::Drag(crossterm::event::MouseButton::Left);
        let mods3 = KeyModifiers::NONE;
        let is_shift_drag3 = matches!(kind3, MouseEventKind::Drag(_))
            && mods3.contains(KeyModifiers::SHIFT);
        assert!(!is_shift_drag3);
    }

    // ── #591 historical-view banner gate ──────────────────────────────────

    /// #591 fix surface: `is_effectively_live` must return `true` when
    /// `Some(a)` is at or past the current `live_anchor`. Before this
    /// helper, the snapshot picked `Option::is_none()` as the only
    /// live-ness signal, so a single mouse-wheel scroll-up that landed at
    /// `Some(a)` would freeze the yellow banner on even after enough new
    /// content streamed in that the visible region was the live tail.
    #[test]
    fn historical_view_banner_hidden_when_at_live_tail() {
        let height = 20usize;
        let mut buf = ScrollbackBuffer::with_capacity(200);
        for i in 0..30u32 {
            buf.push(format!("L{i}"));
        }

        // True live: discriminant is None → live everywhere.
        let live = ScrollbackState::live();
        assert!(live.is_live(), "None discriminant must be is_live()");
        assert!(
            live.is_effectively_live(&buf, height),
            "None discriminant must be is_effectively_live() too"
        );

        // Stored anchor sits BELOW live_anchor — genuine historical view.
        let live_anchor = buf.live_anchor(height);
        assert!(live_anchor > 0, "test pre-cond: buffer must produce a non-zero live anchor");
        let historical = ScrollbackState(Some(live_anchor - 1));
        assert!(!historical.is_live(), "Some(_) is_live() is always false");
        assert!(
            !historical.is_effectively_live(&buf, height),
            "an anchor below live_anchor is genuinely historical and MUST show the banner",
        );

        // Stored anchor sits AT live_anchor — visually live tail, banner
        // must go away. This is the #591 case: user scrolled up by 1, then
        // enough new content arrived that the live anchor caught up.
        let at_live = ScrollbackState(Some(live_anchor));
        assert!(!at_live.is_live(), "literal is_live() still reports historical");
        assert!(
            at_live.is_effectively_live(&buf, height),
            "an anchor AT live_anchor is at the live tail; banner MUST be hidden (#591)",
        );

        // Anchor past live_anchor (buffer grew under a stale Some(a)) —
        // also effectively live.
        let past_live = ScrollbackState(Some(live_anchor + 5));
        assert!(
            past_live.is_effectively_live(&buf, height),
            "an anchor past live_anchor must also gate the banner off",
        );
    }
}
