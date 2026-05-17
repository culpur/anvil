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
