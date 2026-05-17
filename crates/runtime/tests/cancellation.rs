//! v2.2.14 TUI-1: cooperative cancellation of an in-flight turn.
//!
//! The fake `ApiClient` here mimics a streaming provider that yields one
//! frame and, between frames, flips the shared cancel flag the runtime
//! handed it — simulating the TUI's Ctrl+C handler running while the SSE
//! loop is alive. The runtime is expected to short-circuit with
//! `RuntimeError::cancelled()` rather than completing the turn.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use runtime::{
    ApiClient, ApiRequest, AssistantEvent, ConversationRuntime, PermissionMode,
    PermissionPolicy, PromptSection, PromptSectionKind, RuntimeError, Session,
    StaticToolExecutor, TokenUsage,
};

struct CancelMidStreamClient {
    token: Option<Arc<AtomicBool>>,
}

impl ApiClient for CancelMidStreamClient {
    fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        // First frame "received" — now simulate the user pressing Ctrl+C in
        // the TUI by flipping the cancel flag we were handed. The runtime's
        // post-stream check sees this and short-circuits with Cancelled.
        if let Some(t) = &self.token {
            t.store(true, Ordering::SeqCst);
        }
        Ok(vec![
            AssistantEvent::TextDelta("partial".to_string()),
            AssistantEvent::Usage(TokenUsage {
                input_tokens: 5,
                output_tokens: 1,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            }),
            AssistantEvent::MessageStop,
        ])
    }

    fn set_cancel_token(&mut self, token: Arc<AtomicBool>) {
        self.token = Some(token);
    }
}

#[test]
fn cancel_between_frames_returns_cancelled_error() {
    let mut runtime = ConversationRuntime::new(
        Session::new(),
        CancelMidStreamClient { token: None },
        StaticToolExecutor::new(),
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        vec![PromptSection::new(PromptSectionKind::System, "system")],
    );

    let err = runtime
        .run_turn("anything", None)
        .expect_err("cancellation should surface as an error");
    assert!(err.is_cancelled(), "expected Cancelled, got: {err}");
}

struct SingleOkClient {
    called: bool,
}

impl ApiClient for SingleOkClient {
    fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        assert!(!self.called, "should only be called once");
        self.called = true;
        Ok(vec![
            AssistantEvent::TextDelta("ok".to_string()),
            AssistantEvent::MessageStop,
        ])
    }
}

#[test]
fn stale_cancel_flag_does_not_poison_next_turn() {
    // If the cancel flag is already set when run_turn is called, the
    // runtime resets it at dispatch so a previous cancel doesn't kill the
    // following turn.
    let mut runtime = ConversationRuntime::new(
        Session::new(),
        SingleOkClient { called: false },
        StaticToolExecutor::new(),
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        vec![PromptSection::new(PromptSectionKind::System, "system")],
    );
    let handle = runtime.cancel_handle();
    handle.store(true, Ordering::SeqCst);
    let summary = runtime
        .run_turn("hi", None)
        .expect("pre-existing stale flag should be cleared on dispatch");
    assert_eq!(summary.assistant_messages.len(), 1);
}

/// Mock client that yields N chunks and lets the test flip the cancel flag
/// at a configurable iteration. Models the realistic "long stream, user
/// cancels partway" path that the bug was in `DefaultRuntimeClient`.
///
/// NOTE: This is a *trait-contract* test — it proves the runtime correctly
/// surfaces Cancelled when a conformant `ApiClient` impl polls the flag and
/// bails. The end-to-end fix lives in
/// `crates/anvil-cli/src/providers.rs::cancel_token_tests` (DefaultRuntimeClient
/// no longer leaves the trait default no-op).
struct ChunkedCancellableClient {
    token: Option<Arc<AtomicBool>>,
    chunks_yielded: Arc<std::sync::Mutex<usize>>,
    cancel_after_chunk: usize,
    total_chunks: usize,
}

impl ApiClient for ChunkedCancellableClient {
    fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        let mut events = Vec::new();
        for i in 0..self.total_chunks {
            // Poll the cancel flag between each chunk — this is what
            // DefaultRuntimeClient does in the SSE loop. If the flag is
            // set, stop yielding and return Cancelled.
            if let Some(token) = &self.token
                && token.load(Ordering::SeqCst)
            {
                return Err(RuntimeError::cancelled());
            }
            events.push(AssistantEvent::TextDelta(format!("chunk-{i}")));
            *self.chunks_yielded.lock().unwrap() = i + 1;
            // After delivering the target chunk, simulate the TUI flipping
            // the flag (Ctrl+C arrives between this chunk and the next).
            if i == self.cancel_after_chunk
                && let Some(token) = &self.token
            {
                token.store(true, Ordering::SeqCst);
            }
        }
        events.push(AssistantEvent::MessageStop);
        Ok(events)
    }

    fn set_cancel_token(&mut self, token: Arc<AtomicBool>) {
        self.token = Some(token);
    }
}

#[test]
fn conformant_client_stops_streaming_after_cancel_flip() {
    // Set up a client that would yield 10 chunks, but flips its own cancel
    // flag after chunk 2. A conformant `ApiClient` impl (polling the flag
    // between chunks) MUST yield no more than 3 chunks (the one it was
    // delivering when the flip happened, plus the next poll catches it)
    // before returning Cancelled.
    let chunks_yielded = Arc::new(std::sync::Mutex::new(0usize));
    let client = ChunkedCancellableClient {
        token: None,
        chunks_yielded: Arc::clone(&chunks_yielded),
        cancel_after_chunk: 2,
        total_chunks: 10,
    };

    let mut runtime = ConversationRuntime::new(
        Session::new(),
        client,
        StaticToolExecutor::new(),
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        vec![PromptSection::new(PromptSectionKind::System, "system")],
    );
    let err = runtime
        .run_turn("anything", None)
        .expect_err("cancel-after-chunk-2 should surface as Cancelled");
    assert!(err.is_cancelled(), "expected Cancelled, got: {err}");

    let delivered = *chunks_yielded.lock().unwrap();
    assert!(
        delivered <= 3,
        "conformant impl should bail within one poll of the flag flip; \
         got {delivered} chunks (>3 means the impl ignored the cancel flag, \
         which is the v2.2.14 TUI-1 bug we just fixed in DefaultRuntimeClient)"
    );
}

// ---------------------------------------------------------------------------
// Task #605: slow-read cancel responsiveness
//
// The pre-existing `ChunkedCancellableClient` flips its own flag *between*
// fast in-memory iterations — so the cancel poll happens at chunk boundaries
// where no real time has elapsed. The user's v2.2.16-preview screenshot
// captured a different failure mode: a stream blocked on the network read
// itself, where the cancel flag flipped while no chunk was arriving.
//
// In `DefaultRuntimeClient::stream` we patched this by racing `next_event`
// against `wait_for_cancel` inside a `tokio::select!`, so the read future
// is dropped (and the HTTP connection torn down by reqwest's Drop impl) as
// soon as the flag flips. The trait-contract test below captures the SLA
// any conformant `ApiClient` impl must meet: a slow `stream()` invocation
// (modeled here as a real sleep, mirroring socket-blocked time) MUST return
// `Cancelled` within ~200 ms of the flag flipping — NOT after the stream
// completes naturally.
// ---------------------------------------------------------------------------

/// `ApiClient` impl that simulates a real HTTP read taking 1.5 s, then
/// emitting one event. While the read is "in flight" (i.e. inside the
/// `stream()` call), a background watchdog races the cancel flag against
/// the simulated delay — modeling the `tokio::select!` arm in
/// `DefaultRuntimeClient::stream`.
struct SlowReadCancellableClient {
    token: Option<Arc<AtomicBool>>,
    /// Records how long `stream()` blocked before returning. The contract is
    /// "<= 200 ms when cancel flips early" — without the select! arm, this
    /// would be the full 1500 ms read.
    elapsed_ms: Arc<std::sync::Mutex<u128>>,
}

impl ApiClient for SlowReadCancellableClient {
    fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        let start = std::time::Instant::now();
        let token = self.token.clone();
        let elapsed = Arc::clone(&self.elapsed_ms);

        // Use a single-threaded tokio runtime so this test doesn't need to
        // be tagged with #[tokio::test] — matching the architecture of
        // DefaultRuntimeClient which owns its own `tokio::runtime::Runtime`.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test tokio runtime");

        let result = rt.block_on(async move {
            // The "wait_for_cancel" half of the select arm — 50 ms poll.
            let cancel_fut = async {
                let Some(token) = token else {
                    std::future::pending::<()>().await;
                    return;
                };
                loop {
                    if token.load(Ordering::SeqCst) {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            };
            // The "in-flight HTTP read" half — 1.5 s, way longer than any
            // reasonable cancel latency.
            let read_fut = async {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                AssistantEvent::TextDelta("late-frame".to_string())
            };

            tokio::select! {
                biased;
                () = cancel_fut => Err(RuntimeError::cancelled()),
                event = read_fut => Ok(vec![event, AssistantEvent::MessageStop]),
            }
        });

        *elapsed.lock().unwrap() = start.elapsed().as_millis();
        result
    }

    fn set_cancel_token(&mut self, token: Arc<AtomicBool>) {
        self.token = Some(token);
    }
}

#[test]
fn cancel_aborts_blocking_http_read_within_200ms() {
    // Reproduces the #605 user screenshot: a stream is blocked on the next
    // chunk read for >1 s. The TUI flips the cancel flag 100 ms in. The
    // streaming loop MUST return Cancelled within 200 ms of the flip,
    // before the simulated read would have produced a frame.
    //
    // A spawned thread is used to flip the flag mid-stream because
    // `run_turn` is synchronous from the runtime's perspective. This mirrors
    // the TUI's Ctrl+C handler running on its event-loop thread while the
    // provider stream is alive in `block_on`.
    let elapsed = Arc::new(std::sync::Mutex::new(0u128));
    let client = SlowReadCancellableClient {
        token: None,
        elapsed_ms: Arc::clone(&elapsed),
    };

    let mut runtime = ConversationRuntime::new(
        Session::new(),
        client,
        StaticToolExecutor::new(),
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        vec![PromptSection::new(PromptSectionKind::System, "system")],
    );
    let cancel_handle = runtime.cancel_handle();

    // Background "TUI": wait 100 ms, then press Ctrl+C.
    let flipper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        cancel_handle.store(true, Ordering::SeqCst);
    });

    let start = std::time::Instant::now();
    let err = runtime
        .run_turn("anything", None)
        .expect_err("mid-read cancel should surface as Cancelled");
    let total_elapsed_ms = start.elapsed().as_millis();
    flipper.join().expect("flipper thread");

    assert!(err.is_cancelled(), "expected Cancelled, got: {err}");

    // The cancel was flipped at 100 ms. The select! arm has a 50 ms poll
    // interval. So a strict bound is 100 + 50 + scheduling = ~200 ms. We
    // allow 300 ms in the assert to absorb cross-platform test-runner
    // jitter (macOS CI in particular). If this fires, the new select! arm
    // is not actually winning the race against the read — same fiction
    // mode as the #603 first attempt.
    assert!(
        total_elapsed_ms < 300,
        "cancel took {total_elapsed_ms} ms from run_turn start (flag flipped at +100 ms); \
         expected <300 ms — slow cancel means the select! arm is not aborting \
         the read future, which IS the #605 bug"
    );

    // Sanity: the stream() call itself bailed early, not after the 1500 ms
    // simulated read. If this trips, the test fiction is that we measured
    // run_turn timing but stream() actually ran to completion.
    let stream_elapsed_ms = *elapsed.lock().unwrap();
    assert!(
        stream_elapsed_ms < 300,
        "stream() blocked for {stream_elapsed_ms} ms — should have bailed within \
         ~150 ms of the 100 ms cancel flip"
    );
}

/// Like `SlowReadCancellableClient` but with an infinite read — proves that
/// even a stream which would NEVER produce a frame gets cancelled. This is
/// the stronger guarantee #605 promises: cancellation does NOT wait for the
/// next event.
struct NeverReadsClient {
    token: Option<Arc<AtomicBool>>,
}

impl ApiClient for NeverReadsClient {
    fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        let token = self.token.clone();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test tokio runtime");

        rt.block_on(async move {
            let cancel_fut = async {
                let Some(token) = token else {
                    std::future::pending::<()>().await;
                    return;
                };
                loop {
                    if token.load(Ordering::SeqCst) {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            };
            // "Read" that never returns — pending forever — modeling a socket
            // that the server has accepted but is not writing to.
            let read_fut = std::future::pending::<AssistantEvent>();

            tokio::select! {
                biased;
                () = cancel_fut => Err(RuntimeError::cancelled()),
                _event = read_fut => unreachable!("pending future cannot resolve"),
            }
        })
    }

    fn set_cancel_token(&mut self, token: Arc<AtomicBool>) {
        self.token = Some(token);
    }
}

#[test]
fn cancel_during_active_read_does_not_wait_for_next_event() {
    // The strongest #605 guarantee: a stream that would NEVER produce an
    // event still gets cancelled in <200 ms. Before #605, the loop would
    // sit on the next_event() await until the 5-minute stall timeout —
    // that's the actual user-visible failure (multiple Ctrl+C presses
    // doing nothing because the read is the thing that's blocked).
    let mut runtime = ConversationRuntime::new(
        Session::new(),
        NeverReadsClient { token: None },
        StaticToolExecutor::new(),
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        vec![PromptSection::new(PromptSectionKind::System, "system")],
    );
    let cancel_handle = runtime.cancel_handle();

    let flipper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        cancel_handle.store(true, Ordering::SeqCst);
    });

    let start = std::time::Instant::now();
    let err = runtime
        .run_turn("anything", None)
        .expect_err("infinite-read stream must still cancel");
    let elapsed_ms = start.elapsed().as_millis();
    flipper.join().expect("flipper thread");

    assert!(err.is_cancelled(), "expected Cancelled, got: {err}");
    assert!(
        elapsed_ms < 300,
        "infinite-read cancel took {elapsed_ms} ms; expected <300 ms — \
         this is the scenario the user's screenshot captured (multiple \
         ⏸ cancelled messages because the read was blocked forever)"
    );
}
