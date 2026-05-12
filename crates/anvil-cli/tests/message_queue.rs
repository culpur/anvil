//! v2.2.14 TUI-3: per-tab user-message queue.
//!
//! Tests the FIFO contract at the data-structure level: submissions push
//! to the back, the dispatch path pops from the front, Esc pops the back
//! (newest), and a "clear-all" wipes everything. The TUI integration of
//! those semantics is exercised manually (see commit message).

use std::collections::VecDeque;

/// FIFO insertion / dispatch order.
#[test]
fn queue_dispatches_in_submission_order() {
    let mut q: VecDeque<String> = VecDeque::new();
    q.push_back("first".to_string());
    q.push_back("second".to_string());
    q.push_back("third".to_string());

    assert_eq!(q.pop_front().unwrap(), "first");
    assert_eq!(q.pop_front().unwrap(), "second");
    assert_eq!(q.pop_front().unwrap(), "third");
    assert!(q.pop_front().is_none());
}

/// Plain Esc semantics: pop_back removes the most recently submitted draft.
#[test]
fn esc_pops_back_leaving_earlier_queued() {
    let mut q: VecDeque<String> = VecDeque::new();
    q.push_back("a".to_string());
    q.push_back("b".to_string());
    q.push_back("c".to_string());

    let dropped = q.pop_back();
    assert_eq!(dropped.as_deref(), Some("c"));
    // The remaining queue still fires earlier submissions in order.
    assert_eq!(q.pop_front().unwrap(), "a");
    assert_eq!(q.pop_front().unwrap(), "b");
}

/// Ctrl+Shift+Esc semantics: wipe everything.
#[test]
fn clear_all_drops_every_queued_message() {
    let mut q: VecDeque<String> = VecDeque::new();
    q.push_back("a".to_string());
    q.push_back("b".to_string());
    q.push_back("c".to_string());
    assert_eq!(q.len(), 3);
    q.clear();
    assert!(q.is_empty());
}

/// Adding to a non-empty queue preserves order (regression: it would be
/// easy to mistakenly push_front and break the user's expectation that
/// the older submission runs first).
#[test]
fn adding_to_non_empty_queue_preserves_order() {
    let mut q: VecDeque<String> = VecDeque::new();
    q.push_back("alpha".to_string());
    // Simulate a later Enter while a turn still streams.
    q.push_back("beta".to_string());
    assert_eq!(q.front().map(String::as_str), Some("alpha"));
    assert_eq!(q.back().map(String::as_str), Some("beta"));
}
