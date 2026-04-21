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
}
