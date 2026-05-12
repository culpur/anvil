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
    PermissionPolicy, RuntimeError, Session, StaticToolExecutor, TokenUsage,
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
        vec!["system".to_string()],
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
        vec!["system".to_string()],
    );
    let handle = runtime.cancel_handle();
    handle.store(true, Ordering::SeqCst);
    let summary = runtime
        .run_turn("hi", None)
        .expect("pre-existing stale flag should be cleared on dispatch");
    assert_eq!(summary.assistant_messages.len(), 1);
}
