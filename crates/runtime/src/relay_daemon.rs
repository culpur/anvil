//! Daemon-aware feed for Remote Control (task #647).
//!
//! Decoupled from the network and the TUI so it can be unit-tested with
//! pure inputs: feed in a sequence of `(status_json, pending_proposals)`
//! observations and assert what frames the host should push to the
//! viewer over the WebSocket.
//!
//! Two concerns live here:
//!
//! 1. **DaemonStatus de-duplication.** The poller fires every 5 seconds.
//!    99% of ticks produce identical bytes — `anvild.status.json` is
//!    rewritten on every 30 s daemon tick but the content only changes
//!    when routines fire, proposals are written, or errors surface.  We
//!    emit `RelayMessage::DaemonStatus` only when the bytes differ from
//!    the last emit, capped by an idle floor so the viewer never goes
//!    longer than `IDLE_HEARTBEAT_SECS` without a fresh status frame
//!    (lets the browser detect dead host).  This is the BUG-122-8 fix
//!    surface — see `feedback-tui-flash-anti-pattern` for the analogous
//!    in-TUI rule.
//!
//! 2. **Proposal delta coding.** A naive feed re-sends the full proposal
//!    list every poll.  Instead the poller keeps the last-known set
//!    (by routine name) and emits:
//!      * `ProposalSnapshot` once per WebSocket pair (full state).
//!      * `ProposalAdded` for each name that wasn't in the previous set.
//!      * `ProposalDropped` for each name that vanished.
//!
//! Both are *just functions of state* — no I/O, no async — so the unit
//! tests don't need a tokio runtime or a temp dir.

use std::collections::HashSet;

use crate::relay::{ProposalSummary, RelayMessage};

/// How often the host polls anvild status (used by the network task,
/// not enforced here — this module only handles the diff logic).
pub const POLL_INTERVAL_SECS: u64 = 5;

/// Maximum gap between two DaemonStatus emits, regardless of dedupe.
///
/// Even when the body is identical we re-emit at this cadence so the
/// viewer can tell the daemon is alive (and so a viewer that reconnected
/// mid-quiet-stretch sees something within bounded time).
pub const IDLE_HEARTBEAT_SECS: u64 = 60;

/// State carried across polls for the daemon-status feed.
#[derive(Debug, Default, Clone)]
pub struct DaemonFeedState {
    /// Hash of the bytes we last successfully serialized + sent.  None
    /// means we've never emitted.  Cheap memcmp via `String::eq`.
    last_emit_json: Option<String>,
    /// Unix epoch seconds when [`last_emit_json`] was sent.
    last_emit_at: u64,
}

impl DaemonFeedState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide whether the given status should be pushed.
    ///
    /// Returns `Some(message)` when the caller should send, `None` when
    /// the body matches the last emit and the idle heartbeat hasn't
    /// elapsed.
    ///
    /// The caller passes the status fields as a `RelayMessage::DaemonStatus`
    /// value so the dedupe key is the wire form, not an internal struct
    /// that might evolve out from under the comparison.
    pub fn observe(&mut self, msg: RelayMessage, now: u64) -> Option<RelayMessage> {
        let json = serde_json::to_string(&msg).ok()?;
        let same_body = self.last_emit_json.as_deref() == Some(json.as_str());
        let heartbeat_due = self
            .last_emit_at
            .checked_add(IDLE_HEARTBEAT_SECS)
            .map(|deadline| now >= deadline)
            .unwrap_or(true);
        if same_body && !heartbeat_due {
            return None;
        }
        self.last_emit_json = Some(json);
        self.last_emit_at = now;
        Some(msg)
    }

    /// Force the next `observe` to emit, even if the body hasn't changed.
    ///
    /// Called when a new web client pairs — they need at least one
    /// frame to bootstrap their UI.
    pub fn force_next(&mut self) {
        self.last_emit_json = None;
        self.last_emit_at = 0;
    }
}

/// State carried across polls for the proposal feed.
#[derive(Debug, Default, Clone)]
pub struct ProposalFeedState {
    /// Routine names from the previous observation.
    known: HashSet<String>,
    /// True until the first snapshot has been sent on a freshly-paired
    /// connection.
    awaiting_initial_snapshot: bool,
}

impl ProposalFeedState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            known: HashSet::new(),
            awaiting_initial_snapshot: true,
        }
    }

    /// Reset to "freshly paired" — the next observation will emit a
    /// full `ProposalSnapshot` regardless of diff.
    pub fn reset_for_new_pair(&mut self) {
        self.known.clear();
        self.awaiting_initial_snapshot = true;
    }

    /// Diff the current pending list against the known set.
    ///
    /// On the first call after [`new`] / [`reset_for_new_pair`] this
    /// emits a single `ProposalSnapshot`.  Subsequent calls emit a
    /// stream of `ProposalAdded` and `ProposalDropped` messages, one
    /// per changed routine — never a snapshot.
    pub fn observe<'a>(
        &mut self,
        current: impl IntoIterator<Item = &'a ProposalSummary>,
    ) -> Vec<RelayMessage> {
        let current: Vec<ProposalSummary> = current.into_iter().cloned().collect();
        let current_names: HashSet<String> =
            current.iter().map(|p| p.routine.clone()).collect();

        if self.awaiting_initial_snapshot {
            self.awaiting_initial_snapshot = false;
            self.known = current_names;
            return vec![RelayMessage::ProposalSnapshot { proposals: current }];
        }

        let mut out = Vec::new();
        // New (in current but not in known)
        for p in &current {
            if !self.known.contains(&p.routine) {
                out.push(RelayMessage::ProposalAdded {
                    proposal: p.clone(),
                });
            }
        }
        // Dropped (in known but not in current)
        for name in self.known.difference(&current_names) {
            out.push(RelayMessage::ProposalDropped {
                routine: name.clone(),
            });
        }
        self.known = current_names;
        out
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn status(running: bool, pending: usize, last_tick_at: u64) -> RelayMessage {
        RelayMessage::DaemonStatus {
            running,
            pid: if running { Some(1234) } else { None },
            last_tick_at: Some(last_tick_at),
            routines_loaded: 4,
            routines_fired_last_tick: 0,
            pending_proposals_total: pending,
            last_error: None,
            anvil_version: Some("2.2.18-test".into()),
        }
    }

    // ── DaemonFeedState ─────────────────────────────────────────────────

    #[test]
    fn daemon_feed_first_observation_always_emits() {
        let mut s = DaemonFeedState::new();
        let m = status(true, 0, 100);
        assert!(s.observe(m, 100).is_some());
    }

    #[test]
    fn daemon_feed_dedupes_identical_back_to_back() {
        let mut s = DaemonFeedState::new();
        let m1 = status(true, 0, 100);
        let m2 = status(true, 0, 100); // byte-identical
        assert!(s.observe(m1, 100).is_some());
        assert!(s.observe(m2, 105).is_none());
    }

    #[test]
    fn daemon_feed_emits_on_body_change() {
        let mut s = DaemonFeedState::new();
        let _ = s.observe(status(true, 0, 100), 100);
        let again = s.observe(status(true, 1, 130), 130);
        assert!(again.is_some(), "pending_proposals_total changed → emit");
    }

    #[test]
    fn daemon_feed_heartbeat_after_idle() {
        let mut s = DaemonFeedState::new();
        let _ = s.observe(status(true, 0, 100), 100);
        // Same body, but past the idle heartbeat.
        let later = s.observe(status(true, 0, 100), 100 + IDLE_HEARTBEAT_SECS);
        assert!(later.is_some(), "heartbeat should re-emit identical body");
    }

    #[test]
    fn daemon_feed_no_emit_inside_heartbeat_window() {
        let mut s = DaemonFeedState::new();
        let _ = s.observe(status(true, 0, 100), 100);
        // Same body, well within heartbeat.
        for delta in [5, 10, 15, 30, 45] {
            assert!(
                s.observe(status(true, 0, 100), 100 + delta).is_none(),
                "delta={delta} should not emit"
            );
        }
    }

    #[test]
    fn daemon_feed_force_next_emits_even_for_identical_body() {
        let mut s = DaemonFeedState::new();
        let _ = s.observe(status(true, 0, 100), 100);
        s.force_next();
        assert!(
            s.observe(status(true, 0, 100), 105).is_some(),
            "force_next should pop the dedupe gate"
        );
    }

    #[test]
    fn daemon_feed_r6_no_flooding_under_steady_state() {
        // R6 from the audit doc.  Within one heartbeat window, an
        // identical body must emit exactly once regardless of poll rate.
        //
        // Steady-state: identical body, 1-second poll cadence (the
        // anvild loop is 30s but the relay poller is 5s; we use 1s
        // here to maximise tick count without crossing the
        // IDLE_HEARTBEAT_SECS boundary).
        let mut s = DaemonFeedState::new();
        let mut emits = 0;
        let ticks_within_heartbeat = IDLE_HEARTBEAT_SECS;
        for tick in 0..ticks_within_heartbeat {
            if s.observe(status(true, 0, 100), 100 + tick).is_some() {
                emits += 1;
            }
        }
        assert_eq!(
            emits, 1,
            "steady-state daemon-status floods are the BUG-122-8 surface; \
             expected exactly 1 emit in {ticks_within_heartbeat} ticks within \
             one heartbeat window, got {emits}"
        );
    }

    // ── ProposalFeedState ───────────────────────────────────────────────

    fn proposal(name: &str, scheduled_at: u64) -> ProposalSummary {
        ProposalSummary {
            routine: name.into(),
            schedule_raw: "every 1h".into(),
            permission_mode: "accept".into(),
            prompt_preview: "do thing".into(),
            scheduled_at,
            proposed_at: scheduled_at + 1,
        }
    }

    #[test]
    fn proposal_feed_first_observation_is_snapshot() {
        let mut s = ProposalFeedState::new();
        let out = s.observe(&[proposal("a", 100), proposal("b", 200)]);
        assert_eq!(out.len(), 1);
        match &out[0] {
            RelayMessage::ProposalSnapshot { proposals } => {
                assert_eq!(proposals.len(), 2);
            }
            other => panic!("expected ProposalSnapshot, got {other:?}"),
        }
    }

    #[test]
    fn proposal_feed_subsequent_emits_deltas_only() {
        let mut s = ProposalFeedState::new();
        let _ = s.observe(&[proposal("a", 100)]);
        let out = s.observe(&[proposal("a", 100), proposal("b", 200)]);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], RelayMessage::ProposalAdded { .. }));
    }

    #[test]
    fn proposal_feed_emits_drop_when_routine_vanishes() {
        let mut s = ProposalFeedState::new();
        let _ = s.observe(&[proposal("a", 100), proposal("b", 200)]);
        let out = s.observe(&[proposal("a", 100)]); // b dropped
        assert_eq!(out.len(), 1);
        match &out[0] {
            RelayMessage::ProposalDropped { routine } => assert_eq!(routine, "b"),
            other => panic!("expected ProposalDropped, got {other:?}"),
        }
    }

    #[test]
    fn proposal_feed_emits_add_and_drop_together_when_set_swaps() {
        let mut s = ProposalFeedState::new();
        let _ = s.observe(&[proposal("a", 100)]);
        let out = s.observe(&[proposal("b", 200)]);
        assert_eq!(out.len(), 2);
        let adds = out
            .iter()
            .filter(|m| matches!(m, RelayMessage::ProposalAdded { .. }))
            .count();
        let drops = out
            .iter()
            .filter(|m| matches!(m, RelayMessage::ProposalDropped { .. }))
            .count();
        assert_eq!(adds, 1);
        assert_eq!(drops, 1);
    }

    #[test]
    fn proposal_feed_no_changes_emits_nothing() {
        let mut s = ProposalFeedState::new();
        let _ = s.observe(&[proposal("a", 100)]);
        let out = s.observe(&[proposal("a", 100)]);
        assert!(out.is_empty());
    }

    #[test]
    fn proposal_feed_reset_resends_full_snapshot() {
        let mut s = ProposalFeedState::new();
        let _ = s.observe(&[proposal("a", 100)]);
        s.reset_for_new_pair();
        let out = s.observe(&[proposal("a", 100), proposal("b", 200)]);
        assert_eq!(out.len(), 1);
        match &out[0] {
            RelayMessage::ProposalSnapshot { proposals } => {
                assert_eq!(proposals.len(), 2);
            }
            other => panic!("expected ProposalSnapshot after reset, got {other:?}"),
        }
    }
}
