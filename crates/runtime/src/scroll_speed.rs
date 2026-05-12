//! Mouse-wheel scroll speed — process-scoped.
//!
//! CC-139-F3 parity. The TUI reads this on every wheel event to decide
//! how many lines a single tick advances the scrollback (or any
//! scrollable overlay).
//!
//! Default is 3 lines (matches prior hardcoded behaviour).  Clamp is
//! [1, 10].

use std::sync::atomic::{AtomicU8, Ordering};

const DEFAULT_LINES: u8 = 3;
const MIN_LINES: u8 = 1;
const MAX_LINES: u8 = 10;

static CURRENT: AtomicU8 = AtomicU8::new(DEFAULT_LINES);

/// Read the current wheel-tick line count.
#[must_use]
pub fn get_scroll_speed() -> u8 {
    let raw = CURRENT.load(Ordering::Relaxed);
    if raw == 0 { DEFAULT_LINES } else { raw }
}

/// Set the wheel-tick line count, clamped to [1, 10].  Values outside
/// the range silently clamp rather than panic — slash-command handler
/// is responsible for rejecting bad user input before calling.
pub fn set_scroll_speed(lines: u8) {
    let clamped = lines.clamp(MIN_LINES, MAX_LINES);
    CURRENT.store(clamped, Ordering::Relaxed);
}

/// Reset to the default (used by `/scroll-speed reset` if ever wired).
pub fn reset_scroll_speed() {
    CURRENT.store(DEFAULT_LINES, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_three_lines() {
        // Reset to default (other tests may have mutated it).
        reset_scroll_speed();
        assert_eq!(get_scroll_speed(), DEFAULT_LINES);
    }

    #[test]
    fn set_within_range_round_trips() {
        set_scroll_speed(5);
        assert_eq!(get_scroll_speed(), 5);
        set_scroll_speed(1);
        assert_eq!(get_scroll_speed(), 1);
        set_scroll_speed(10);
        assert_eq!(get_scroll_speed(), 10);
    }

    #[test]
    fn set_clamps_low_to_one() {
        set_scroll_speed(0);
        assert_eq!(get_scroll_speed(), MIN_LINES);
    }

    #[test]
    fn set_clamps_high_to_ten() {
        set_scroll_speed(99);
        assert_eq!(get_scroll_speed(), MAX_LINES);
    }
}
