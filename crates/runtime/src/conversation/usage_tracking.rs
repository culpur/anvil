//! Usage aggregation during conversation turns.
//!
//! Provides helpers for recording `TokenUsage` from assistant events into the
//! `UsageTracker`, keeping usage accounting isolated from turn orchestration.

use crate::usage::{TokenUsage, UsageTracker};

/// Record a `TokenUsage` value into the tracker.
///
/// Called by the turn executor once per API response after extracting usage
/// from the `AssistantEvent` stream.
pub(super) fn collect_and_record(tracker: &mut UsageTracker, usage: TokenUsage) {
    tracker.record(usage);
}
