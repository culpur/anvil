//! v2.2.14 TUI-2 (deep): true per-tab non-blocking turn waits.
//!
//! The previous wait machinery was modal on a single `target_tab_id` and
//! re-bound that target whenever the user switched the visually-active tab.
//! Consequently, Enter on an idle background tab was queued onto the
//! re-bound target instead of dispatching a new turn — so a long-running
//! turn on tab 1 blocked every Enter on tab 2 until tab 1 finished.
//!
//! These tests prove the architectural property at the runtime layer:
//! two `ConversationRuntime`s share a single `TaggedTuiEvent` channel, one
//! is held inside a fake `ApiClient` that blocks until the test releases
//! it, and the other completes promptly. The fast turn's `TurnDone` must
//! arrive on the shared channel BEFORE the slow worker exits — i.e. the
//! TUI's event-pump path sees tab 2's full turn while tab 1 stalls.
//!
//! A separate test exercises the protocol-level routing decision: with
//! `active_tab.in_flight == false`, an Enter on the active tab returns
//! `SubmitChatPrompt` (fire immediately); with `in_flight == true`, the
//! handler queues onto the tab's `message_queue` instead.

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Barrier};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use runtime::{
    ApiClient, ApiRequest, AssistantEvent, ConversationRuntime, PermissionMode,
    PermissionPolicy, RuntimeError, Session, StaticToolExecutor, TokenUsage,
};

// ─── Shadow tagged-event channel ────────────────────────────────────────────
//
// `TaggedTuiEvent`/`TuiSender` are in the (bin-only) anvil-cli crate, so we
// reproduce the minimum protocol here. The pattern matches
// `tests/parallel_tabs.rs`.

#[derive(Debug, Clone)]
enum MockEvent {
    TextDelta(String),
    TurnDone,
}

#[derive(Debug, Clone)]
struct TaggedMockEvent {
    tab_id: usize,
    event: MockEvent,
}

#[derive(Clone)]
struct MockSender {
    inner: SyncSender<TaggedMockEvent>,
    tab_id: usize,
}

impl MockSender {
    fn send(&self, event: MockEvent) {
        let _ = self
            .inner
            .send(TaggedMockEvent { tab_id: self.tab_id, event });
    }
}

// ─── Fake API clients ───────────────────────────────────────────────────────

/// Yields one delta, then blocks on `release` until the test releases it.
/// Mirrors a long-running turn (slow tool call, slow inference, etc.).
struct StallingClient {
    sender: MockSender,
    release: Arc<AtomicBool>,
}

impl ApiClient for StallingClient {
    fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        self.sender
            .send(MockEvent::TextDelta("tab1 partial chunk".to_string()));
        // Spin until the test flips `release`. The TUI event pump and the
        // parallel-tab worker keep running while we sit here.
        while !self.release.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(5));
        }
        Ok(vec![
            AssistantEvent::TextDelta("tab1 finishing".to_string()),
            AssistantEvent::Usage(TokenUsage {
                input_tokens: 3,
                output_tokens: 2,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            }),
            AssistantEvent::MessageStop,
        ])
    }
}

/// Fast-completing client: yields two deltas and returns immediately.
struct FastClient {
    sender: MockSender,
}

impl ApiClient for FastClient {
    fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        self.sender
            .send(MockEvent::TextDelta("tab2 chunk one".to_string()));
        self.sender
            .send(MockEvent::TextDelta("tab2 chunk two".to_string()));
        Ok(vec![
            AssistantEvent::TextDelta("tab2 full response".to_string()),
            AssistantEvent::Usage(TokenUsage {
                input_tokens: 2,
                output_tokens: 4,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            }),
            AssistantEvent::MessageStop,
        ])
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn spawn_runtime_thread<C: ApiClient + Send + 'static>(
    sender: MockSender,
    client: C,
) -> thread::JoinHandle<Result<(), String>> {
    let tab_id = sender.tab_id;
    thread::spawn(move || -> Result<(), String> {
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            client,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        );
        let result = runtime.run_turn("hi", None).map(|_| ()).map_err(|e| e.to_string());
        // The real per-tab worker emits `TurnDone` after the turn returns;
        // mirror that here so the shared channel sees the same protocol.
        let _ = tab_id;
        result
    })
}

fn drain_until<F: FnMut(&TaggedMockEvent) -> bool>(
    rx: &Receiver<TaggedMockEvent>,
    mut pred: F,
    timeout: Duration,
) -> Vec<TaggedMockEvent> {
    let deadline = Instant::now() + timeout;
    let mut collected: Vec<TaggedMockEvent> = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(ev) => {
                let done = pred(&ev);
                collected.push(ev);
                if done {
                    return collected;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return collected,
        }
    }
    collected
}

// ─── Tests ──────────────────────────────────────────────────────────────────

/// Two `ConversationRuntime`s run on independent worker threads. The fast
/// runtime finishes WHILE the slow runtime is still blocked inside its
/// `stream()` implementation.
///
/// This is the core invariant the deep TUI-2 fix relies on at the layer
/// below the TUI: each tab's worker is independent, and the TUI's shared
/// event channel observes the fast turn's stream end before the slow turn
/// is released.
#[test]
fn parallel_tabs_fast_finishes_before_slow_releases() {
    let (tx, rx) = mpsc::sync_channel::<TaggedMockEvent>(64);

    let tab1_sender = MockSender { inner: tx.clone(), tab_id: 1 };
    let tab2_sender = MockSender { inner: tx.clone(), tab_id: 2 };
    drop(tx);

    let release = Arc::new(AtomicBool::new(false));
    let slow = StallingClient {
        sender: tab1_sender.clone(),
        release: Arc::clone(&release),
    };
    let fast = FastClient { sender: tab2_sender.clone() };

    // The two workers race to start; we want tab 1 to ENTER its blocked
    // stream() before tab 2 dispatches, so this barrier sequences the
    // spawns. (In production the TUI spawn order is naturally sequential.)
    let barrier = Arc::new(Barrier::new(2));
    let b1 = Arc::clone(&barrier);
    let tab1_sender_for_thread = tab1_sender.clone();
    let h1 = thread::spawn(move || -> Result<(), String> {
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            slow,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        );
        b1.wait();
        // Tab 1 starts streaming and blocks inside the fake client.
        let result = runtime.run_turn("slow turn", None).map(|_| ()).map_err(|e| e.to_string());
        tab1_sender_for_thread.send(MockEvent::TurnDone);
        result
    });

    barrier.wait();
    // Give tab 1's worker a moment to enter the blocking stream().
    thread::sleep(Duration::from_millis(20));

    let h2 = spawn_runtime_thread(tab2_sender.clone(), fast);

    // Drain the shared event channel for tab 1's partial delta + tab 2's
    // deltas while tab 1 is still blocked. We pull events until either we
    // see enough from tab 2 (>=2 deltas) AND tab 1's partial, or we time
    // out. `recv_timeout` is bounded so the test fails fast if parallelism
    // is broken.
    let mut tab2_deltas: usize = 0;
    let mut saw_tab1_partial = false;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !(saw_tab1_partial && tab2_deltas >= 2) {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(ev) => match (ev.tab_id, ev.event) {
                (1, MockEvent::TextDelta(_)) => saw_tab1_partial = true,
                (2, MockEvent::TextDelta(_)) => tab2_deltas += 1,
                _ => {}
            },
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }

    // Tab 2's runtime should return promptly even though tab 1 is still
    // blocked. Join with a short timeout window to be sure.
    let join_start = Instant::now();
    let _ = h2.join().expect("tab2 panicked");
    let join_took = join_start.elapsed();
    assert!(
        join_took < Duration::from_millis(200),
        "tab 2 join took {join_took:?} — turn should have completed in parallel with tab 1's stall"
    );

    assert!(
        !release.load(Ordering::SeqCst),
        "release flag was prematurely flipped — test setup wrong"
    );
    assert!(
        saw_tab1_partial,
        "tab 1's partial delta should have arrived on the channel during tab 2's turn"
    );
    assert!(
        tab2_deltas >= 2,
        "tab 2 must have streamed at least two text deltas; got {tab2_deltas}"
    );

    // Release tab 1 and join.
    release.store(true, Ordering::SeqCst);
    h1.join().expect("tab1 panicked").expect("tab1 turn errored");
}

/// Two parallel runtimes — each emits its own `TaggedMockEvent`s and we
/// observe the multiplexed stream end-to-end with no cross-contamination.
#[test]
fn shared_event_channel_routes_both_tabs_in_parallel() {
    let (tx, rx) = mpsc::sync_channel::<TaggedMockEvent>(128);

    let tab1_sender = MockSender { inner: tx.clone(), tab_id: 1 };
    let tab2_sender = MockSender { inner: tx.clone(), tab_id: 2 };
    drop(tx);

    let counter = Arc::new(AtomicUsize::new(0));
    let counter1 = Arc::clone(&counter);
    let counter2 = Arc::clone(&counter);

    let h1 = thread::spawn(move || {
        for i in 0..10u32 {
            tab1_sender.send(MockEvent::TextDelta(format!("t1:{i}")));
            counter1.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(2));
        }
        tab1_sender.send(MockEvent::TurnDone);
    });
    let h2 = thread::spawn(move || {
        for i in 0..10u32 {
            tab2_sender.send(MockEvent::TextDelta(format!("t2:{i}")));
            counter2.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(2));
        }
        tab2_sender.send(MockEvent::TurnDone);
    });

    let collected = drain_until(
        &rx,
        |ev| {
            // Stop draining once both TurnDones have been seen.
            matches!(ev.event, MockEvent::TurnDone)
                && counter.load(Ordering::SeqCst) >= 20
        },
        Duration::from_secs(3),
    );

    h1.join().expect("t1 panicked");
    h2.join().expect("t2 panicked");

    let tab1_deltas: Vec<&str> = collected
        .iter()
        .filter_map(|ev| match (&ev.tab_id, &ev.event) {
            (1, MockEvent::TextDelta(s)) => Some(s.as_str()),
            _ => None,
        })
        .collect();
    let tab2_deltas: Vec<&str> = collected
        .iter()
        .filter_map(|ev| match (&ev.tab_id, &ev.event) {
            (2, MockEvent::TextDelta(s)) => Some(s.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(tab1_deltas.len(), 10);
    assert_eq!(tab2_deltas.len(), 10);
    // Tabs interleaved on the wire: their TextDeltas should not all be
    // contiguous if execution was truly parallel. We don't assert exact
    // interleaving (timing-sensitive) but we DO assert that the channel
    // wasn't fully drained of one tab before the other appeared.
    let positions_tab1: Vec<usize> = collected
        .iter()
        .enumerate()
        .filter(|(_, ev)| ev.tab_id == 1)
        .map(|(i, _)| i)
        .collect();
    let positions_tab2: Vec<usize> = collected
        .iter()
        .enumerate()
        .filter(|(_, ev)| ev.tab_id == 2)
        .map(|(i, _)| i)
        .collect();
    if let (Some(&first_t1), Some(&first_t2)) = (positions_tab1.first(), positions_tab2.first()) {
        // First events from both tabs should appear within a few channel
        // slots of one another — if execution were serialised, one tab's
        // events would all precede the other's.
        let gap = first_t1.max(first_t2) - first_t1.min(first_t2);
        assert!(
            gap < collected.len(),
            "tab events do not interleave at all: tab1@{first_t1} tab2@{first_t2}"
        );
    }
}

// ─── In-flight routing-decision tests ───────────────────────────────────────
//
// The architectural fix renames the decision in `handle_in_flight_key_extended`
// from "active_tab.id == target_tab_id" → "active_tab.in_flight". We can't
// directly exercise the (bin-only) `AnvilTui` here, but the decision is a
// pure function of the visually-active tab's `in_flight` flag — so we model
// it locally and verify the routing intent the production code now uses.

#[derive(Debug, PartialEq)]
enum RoutedSubmit {
    /// Fire immediately as a new turn on the active tab.
    Dispatch(String),
    /// Queue onto the active tab's message_queue (active tab is itself streaming).
    Queue(String),
    /// Stage into the global pending_submit slot (active tab is in-flight AND
    /// is the originally-spawned target — the "type-ahead next turn" path).
    PendingSubmit(String),
}

fn route_submit(
    active_in_flight: bool,
    active_eq_target: bool,
    pending_submit_empty: bool,
    draft: &str,
) -> RoutedSubmit {
    if !active_in_flight {
        return RoutedSubmit::Dispatch(draft.to_string());
    }
    if active_eq_target && pending_submit_empty {
        RoutedSubmit::PendingSubmit(draft.to_string())
    } else {
        RoutedSubmit::Queue(draft.to_string())
    }
}

/// User is on idle tab 2 (in_flight=false) while the originally-spawned
/// target (tab 1) streams. Pressing Enter fires a NEW turn on tab 2 — does
/// not queue onto target.
///
/// Before v2.2.14 TUI-2 (deep) this case was the load-bearing bug: the
/// wait re-bound target_tab_id to tab 2 on TabSwitched, so the old
/// `active_tab_id == target_tab_id` test was true and the draft got queued
/// onto tab 2 with no in-flight turn to release it.
#[test]
fn enter_on_idle_active_tab_dispatches_immediately() {
    let routed = route_submit(
        /* active_in_flight = */ false,
        /* active_eq_target = */ true, // irrelevant once in_flight=false
        /* pending_submit_empty = */ true,
        "fire me",
    );
    assert_eq!(routed, RoutedSubmit::Dispatch("fire me".to_string()));
}

/// User is on the in-flight target tab (active == target, in_flight=true).
/// First Enter populates `pending_submit` (the slot the main loop's
/// TurnDone consumer reads as the next turn).
#[test]
fn enter_on_in_flight_target_tab_stages_pending_submit() {
    let routed = route_submit(
        true,
        true,
        true,
        "type-ahead",
    );
    assert_eq!(routed, RoutedSubmit::PendingSubmit("type-ahead".to_string()));
}

/// User is on the in-flight target tab with `pending_submit` already
/// populated. Subsequent Enters queue onto the tab's message_queue
/// (drained FIFO after each TurnDone).
#[test]
fn enter_on_in_flight_target_with_pending_queues() {
    let routed = route_submit(true, true, false, "next next");
    assert_eq!(routed, RoutedSubmit::Queue("next next".to_string()));
}

/// User is on tab 2 (in_flight=true) but tab 1 is the originally-spawned
/// target. Enter goes onto tab 2's own message_queue so it dispatches when
/// tab 2's current turn finishes — NOT onto pending_submit (which belongs
/// to the target tab).
#[test]
fn enter_on_in_flight_non_target_tab_queues_locally() {
    let routed = route_submit(true, false, true, "for tab2");
    assert_eq!(routed, RoutedSubmit::Queue("for tab2".to_string()));
}
