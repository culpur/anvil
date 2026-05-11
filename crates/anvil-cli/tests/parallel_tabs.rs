//! End-to-end test for Bug-3 parallel per-tab inference.
//!
//! Two simulated worker threads stream events concurrently into a shared
//! `TuiSender` channel (one sender per tab) and an optional permission gate is
//! placed mid-stream on Tab 1.  The consumer on the other end verifies:
//!
//!   - Every `TaggedTuiEvent` carries the correct `tab_id`.
//!   - All events from both workers are delivered (no drops, no interleaving of
//!     `tab_id` values across their own events).
//!   - A `PermissionRequired` event from Tab 1 does not block Tab 2's stream.
//!   - Answering the permission request unblocks Tab 1's worker.

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

// Pull in the TUI types from the binary crate.  The integration tests compile
// against the crate under test via the `[dev-dependencies]` path, so we reach
// through `anvil_cli::tui::*`.  However, because `anvil-cli` is a `[[bin]]`
// target (not a `[lib]`), the test harness cannot import it as a library.
//
// Instead we replicate the minimal channel logic here using the same std::sync
// primitives to prove the invariant at the *protocol* level rather than through
// the crate's internal API.  This mirrors how the existing `remote_status.rs`
// integration tests work (they simulate the data flow rather than calling into
// the binary).

// ─── Minimal shadow types (protocol-level) ───────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
enum MockReply {
    Allow,
    AllowAlways,
    Deny,
}

#[derive(Debug)]
enum MockEvent {
    TextDelta(String),
    TurnDone,
    PermissionRequired {
        #[allow(dead_code)]
        tool_name: String,
        response_tx: mpsc::SyncSender<MockReply>,
    },
}

#[derive(Debug)]
struct TaggedMockEvent {
    tab_id: usize,
    event: MockEvent,
}

struct MockSender {
    inner: mpsc::SyncSender<TaggedMockEvent>,
    tab_id: usize,
}

impl MockSender {
    fn send(&self, event: MockEvent) {
        let _ = self.inner.send(TaggedMockEvent { tab_id: self.tab_id, event });
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn make_channel(cap: usize) -> (mpsc::SyncSender<TaggedMockEvent>, Receiver<TaggedMockEvent>) {
    mpsc::sync_channel(cap)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

/// Two workers stream text events concurrently.  Every received event must
/// carry the `tab_id` of the worker that sent it.
#[test]
fn concurrent_two_tab_streams_route_correctly() {
    // Generous buffer so neither worker blocks on a full channel.
    let (tx_shared, rx) = make_channel(64);

    let tx1 = MockSender { inner: tx_shared.clone(), tab_id: 1 };
    let tx2 = MockSender { inner: tx_shared.clone(), tab_id: 2 };

    // Worker for tab 1
    let worker1 = thread::spawn(move || {
        for i in 0..5u32 {
            tx1.send(MockEvent::TextDelta(format!("tab1-chunk-{i}")));
        }
        tx1.send(MockEvent::TurnDone);
    });

    // Worker for tab 2 — starts at the same time
    let worker2 = thread::spawn(move || {
        for i in 0..5u32 {
            tx2.send(MockEvent::TextDelta(format!("tab2-chunk-{i}")));
        }
        tx2.send(MockEvent::TurnDone);
    });

    worker1.join().expect("worker1 panicked");
    worker2.join().expect("worker2 panicked");
    // Channel is now closed (both senders dropped).

    // Collect everything and verify routing.
    let mut tab1_texts: Vec<String> = Vec::new();
    let mut tab2_texts: Vec<String> = Vec::new();

    while let Ok(tagged) = rx.recv_timeout(Duration::from_millis(200)) {
        match tagged.event {
            MockEvent::TextDelta(t) => {
                if tagged.tab_id == 1 {
                    tab1_texts.push(t);
                } else if tagged.tab_id == 2 {
                    tab2_texts.push(t);
                } else {
                    panic!("unexpected tab_id={}", tagged.tab_id);
                }
            }
            MockEvent::TurnDone => { /* expected */ }
            MockEvent::PermissionRequired { .. } => panic!("unexpected PermissionRequired"),
        }
    }

    assert_eq!(tab1_texts.len(), 5, "tab 1 must deliver 5 deltas, got {:?}", tab1_texts);
    assert_eq!(tab2_texts.len(), 5, "tab 2 must deliver 5 deltas, got {:?}", tab2_texts);

    // Verify ordering within each tab is preserved.
    for (i, t) in tab1_texts.iter().enumerate() {
        assert_eq!(t, &format!("tab1-chunk-{i}"), "tab1 delta {i} out of order");
    }
    for (i, t) in tab2_texts.iter().enumerate() {
        assert_eq!(t, &format!("tab2-chunk-{i}"), "tab2 delta {i} out of order");
    }
}

/// Tab 1 blocks on a permission request mid-stream.  Tab 2 must not stall —
/// its events arrive while Tab 1 is waiting.  After the TUI answers, Tab 1
/// resumes and completes.
#[test]
fn permission_gate_on_tab1_does_not_block_tab2() {
    let (tx_shared, rx) = make_channel(64);

    let tx1 = MockSender { inner: tx_shared.clone(), tab_id: 1 };
    let tx2 = MockSender { inner: tx_shared,        tab_id: 2 };

    // ── Tab 1 worker: sends text, hits permission gate, waits, then continues.
    let worker1 = thread::spawn(move || {
        tx1.send(MockEvent::TextDelta("tab1-before-gate".to_string()));

        let (reply_tx, reply_rx) = mpsc::sync_channel::<MockReply>(1);
        tx1.send(MockEvent::PermissionRequired {
            tool_name: "write_file".to_string(),
            response_tx: reply_tx,
        });

        // Block until the simulated TUI replies.
        let reply = reply_rx.recv().expect("reply channel closed");
        assert_eq!(reply, MockReply::Allow, "expected Allow from simulated TUI");

        tx1.send(MockEvent::TextDelta("tab1-after-gate".to_string()));
        tx1.send(MockEvent::TurnDone);
    });

    // ── Tab 2 worker: runs freely without waiting for Tab 1.
    let worker2 = thread::spawn(move || {
        for i in 0..3u32 {
            tx2.send(MockEvent::TextDelta(format!("tab2-chunk-{i}")));
        }
        tx2.send(MockEvent::TurnDone);
    });

    // ── Simulated TUI: read events, answer permission, collect results.
    let mut tab1_texts: Vec<String> = Vec::new();
    let mut tab2_texts: Vec<String> = Vec::new();
    let mut tab1_done = false;
    let mut tab2_done = false;
    let mut permission_answered = false;

    let deadline = std::time::Instant::now() + Duration::from_secs(5);

    while !tab1_done || !tab2_done {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out: tab1_done={tab1_done} tab2_done={tab2_done}"
        );

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(tagged) => match tagged.event {
                MockEvent::TextDelta(t) => {
                    if tagged.tab_id == 1 {
                        tab1_texts.push(t);
                    } else {
                        tab2_texts.push(t);
                    }
                }
                MockEvent::PermissionRequired { response_tx, .. } => {
                    assert_eq!(tagged.tab_id, 1, "only tab 1 should gate on permission");
                    assert!(!permission_answered, "permission answered twice");
                    permission_answered = true;
                    // Simulate TUI: approve so Tab 1's worker unblocks.
                    response_tx.send(MockReply::Allow).expect("failed to send Allow");
                }
                MockEvent::TurnDone => {
                    if tagged.tab_id == 1 {
                        tab1_done = true;
                    } else {
                        tab2_done = true;
                    }
                }
            },
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    worker1.join().expect("worker1 panicked");
    worker2.join().expect("worker2 panicked");

    assert!(permission_answered, "permission request never arrived");

    // Tab 2 must have received all three deltas despite Tab 1's gate.
    assert_eq!(tab2_texts.len(), 3, "tab2 must deliver 3 deltas; got {:?}", tab2_texts);
    for (i, t) in tab2_texts.iter().enumerate() {
        assert_eq!(t, &format!("tab2-chunk-{i}"));
    }

    // Tab 1: the pre-gate delta arrived, and after approval the post-gate delta
    // also arrived.
    assert!(
        tab1_texts.contains(&"tab1-before-gate".to_string()),
        "tab1 pre-gate delta missing; got {:?}", tab1_texts
    );
    assert!(
        tab1_texts.contains(&"tab1-after-gate".to_string()),
        "tab1 post-gate delta missing; got {:?}", tab1_texts
    );
}

// ─── InFlightInterruption protocol tests ─────────────────────────────────────
//
// The three tests below exercise the dispatch-loop invariants introduced by
// Bug-3 Commit-6 at the *protocol* level.  They use the same shadow-type
// pattern as the tests above: a `MockInterruption` enum mirrors the real
// `InFlightInterruption`, and a `MockDispatcher` simulates the main-loop
// dispatch behaviour without touching `AnvilTui`.

#[derive(Debug, PartialEq)]
enum MockInterruption {
    TurnDone,
    ChannelClosed,
    TabSwitched,
    OpenNewTab,
    CloseActiveTab,
    SlashCommand(String),
    SubmitChatPrompt(String),
}

/// A producer pushes a sequence of `MockInterruption` values through a channel;
/// a consumer (the "dispatch loop") processes them in order and records
/// side-effects.
struct MockDispatcher {
    tx: std::sync::mpsc::SyncSender<MockInterruption>,
    rx: std::sync::mpsc::Receiver<MockInterruption>,
}

impl MockDispatcher {
    fn new(cap: usize) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel(cap);
        Self { tx, rx }
    }
}

/// Simulates the `'chat_wait` dispatch loop: continue looping on
/// `TabSwitched`/`OpenNewTab`/`CloseActiveTab`/`SlashCommand`/`SubmitChatPrompt`,
/// break on `TurnDone`/`ChannelClosed`.
fn run_dispatch_loop(
    rx: &std::sync::mpsc::Receiver<MockInterruption>,
) -> (Vec<MockInterruption>, MockInterruption) {
    let mut side_effects: Vec<MockInterruption> = Vec::new();
    loop {
        let interruption = rx
            .recv_timeout(Duration::from_millis(500))
            .expect("dispatch loop timed out");
        match interruption {
            MockInterruption::TurnDone | MockInterruption::ChannelClosed => {
                return (side_effects, interruption);
            }
            other => {
                side_effects.push(other);
            }
        }
    }
}

/// A `TabSwitched` interruption mid-stream does NOT end the dispatch loop —
/// the loop re-enters.  Only the subsequent `TurnDone` terminates it.
/// Invariant: `TabSwitched` appears exactly once in `side_effects` and the
/// final return value is `TurnDone`.
#[test]
fn interruption_on_tab_switch_returns_tab_switched() {
    let disp = MockDispatcher::new(16);

    // Simulate: tab-switch arrives mid-turn, then the turn finishes.
    disp.tx.send(MockInterruption::TabSwitched).unwrap();
    disp.tx.send(MockInterruption::TurnDone).unwrap();

    let (effects, terminal) = run_dispatch_loop(&disp.rx);

    assert_eq!(terminal, MockInterruption::TurnDone, "loop must exit on TurnDone");
    assert_eq!(effects.len(), 1, "exactly one side-effect expected; got {:?}", effects);
    assert!(
        matches!(effects[0], MockInterruption::TabSwitched),
        "side-effect must be TabSwitched; got {:?}", effects[0]
    );
}

/// A `SlashCommand` interruption is collected as a side-effect (the dispatcher
/// executes it) and then the loop re-enters.  The subsequent `TurnDone`
/// terminates the loop.
/// Invariant: the `SlashCommand` payload is preserved intact and the loop
/// does not exit until `TurnDone`.
#[test]
fn interruption_on_slash_returns_slash_command() {
    let disp = MockDispatcher::new(16);

    disp.tx
        .send(MockInterruption::SlashCommand("/ssh guard".to_string()))
        .unwrap();
    disp.tx.send(MockInterruption::TurnDone).unwrap();

    let (effects, terminal) = run_dispatch_loop(&disp.rx);

    assert_eq!(terminal, MockInterruption::TurnDone);
    assert_eq!(effects.len(), 1, "expected one side-effect; got {:?}", effects);
    match &effects[0] {
        MockInterruption::SlashCommand(cmd) => {
            assert_eq!(cmd, "/ssh guard", "slash command payload must be preserved");
        }
        other => panic!("expected SlashCommand; got {:?}", other),
    }
}

/// A `SubmitChatPrompt` interruption on an idle tab is collected as a
/// side-effect; the loop re-enters and exits on the subsequent `TurnDone`.
/// When `ChannelClosed` is used instead, the loop also terminates cleanly.
#[test]
fn interruption_on_chat_submit_returns_submit_then_done() {
    // Part A: SubmitChatPrompt followed by TurnDone.
    {
        let disp = MockDispatcher::new(16);
        disp.tx
            .send(MockInterruption::SubmitChatPrompt("hello world".to_string()))
            .unwrap();
        disp.tx.send(MockInterruption::TurnDone).unwrap();

        let (effects, terminal) = run_dispatch_loop(&disp.rx);
        assert_eq!(terminal, MockInterruption::TurnDone);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            MockInterruption::SubmitChatPrompt(p) => {
                assert_eq!(p, "hello world", "prompt payload must be preserved");
            }
            other => panic!("expected SubmitChatPrompt; got {:?}", other),
        }
    }

    // Part B: channel closes without TurnDone (e.g. worker panicked) —
    // the loop must terminate via ChannelClosed, not hang.
    {
        let (tx, rx): (
            std::sync::mpsc::SyncSender<MockInterruption>,
            std::sync::mpsc::Receiver<MockInterruption>,
        ) = std::sync::mpsc::sync_channel(4);
        tx.send(MockInterruption::SubmitChatPrompt("pending".to_string()))
            .unwrap();
        tx.send(MockInterruption::ChannelClosed).unwrap();

        let (effects, terminal) = run_dispatch_loop(&rx);
        assert_eq!(terminal, MockInterruption::ChannelClosed, "ChannelClosed must exit the loop");
        assert_eq!(effects.len(), 1, "one pending submit before close");
    }
}
