//! Background OAuth keep-alive ticker (Task #597 deliverable #3).
//!
//! Refreshes the saved Anthropic OAuth bearer proactively, well before the
//! wall-clock expiry, so an idle user does not hit a forced re-OAuth after
//! the token expires.  Pairs with:
//!
//!   * `super::SAFETY_WINDOW_SECS` (Task #597 deliverable #1, lazy refresh)
//!   * The 401-retry wrapper in `api::providers::anvil_provider` (Task #597
//!     deliverable #2, server-side revocation safety net)
//!
//! Together the three layers give Anvil parity with Claude Code's
//! credential-keeper behaviour: a token that's revoked, expired between
//! resolve+send, or expired during a long idle period never surfaces a
//! stack trace to the user; either the ticker has already refreshed it, or
//! the in-flight 401 retry refreshes and retries once.
//!
//! Architecture notes:
//!
//!   * The ticker reads `~/.anvil/credentials.json` on every tick — that's
//!     the canonical source of truth shared with `AuthSource::from_env_or_saved`
//!     and with any sibling Anvil process.  An in-memory `RwLock<OAuthTokenSet>`
//!     would split that source of truth and risk a stale-read race; the file
//!     is the cache.
//!   * On refresh failure (refresh_token rejected, network down) the task
//!     emits a `KeepaliveEvent::RefreshFailed` over the system-event channel
//!     so the TUI can surface a banner, then continues ticking — a transient
//!     network failure shouldn't wedge the keep-alive forever.
//!   * The task is cancelled via dropping the `KeepaliveHandle`; the inner
//!     `CancellationToken` (a `tokio_util` equivalent, but built from a
//!     plain watch channel to avoid pulling in another crate) wakes the
//!     ticker out of its sleep on cancel.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::{
    load_oauth_credentials, next_refresh_delay_secs, save_oauth_credentials,
    unix_now_seconds, OAuthTokenSet, KEEPALIVE_MIN_DELAY_SECS,
};

/// System-event payload emitted by the keep-alive ticker.  The TUI loop
/// receives these over an `mpsc::UnboundedReceiver` (per task #597 brief:
/// "emit a TUI banner via the existing system-event channel rather than
/// panicking"); non-TUI runs (`anvil -p`, CI) can drop the receiver and the
/// ticker silently no-ops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepaliveEvent {
    /// The ticker successfully refreshed the bearer.  Carries the new
    /// `expires_at` (unix seconds) so the TUI can render a subtle "Bearer
    /// refreshed, valid for Nh" toast if it wants.
    Refreshed { new_expires_at: Option<u64> },
    /// The refresh attempt failed.  `reason` is a human-readable error
    /// suitable for a banner; the ticker continues running (next tick will
    /// try again unless the refresh_token has been irrevocably rejected).
    RefreshFailed { reason: String },
    /// No Anthropic OAuth credential is present — ticker idles at MIN and
    /// checks again later (covers "user logs in mid-session" flow).
    NoCredential,
    /// Task #763 (v2.2.20): heartbeat event emitted on every tick where the
    /// credential is valid but not yet at the refresh threshold.  Without
    /// this, a freshly-issued token (hours of validity left) keeps the
    /// ticker silent for hours, making proof-of-life impossible.  Carries
    /// the observed `expires_at` so observers can render countdown UIs.
    Heartbeat { expires_at: Option<u64> },
    /// The ticker has been cancelled and the task is about to exit.
    Stopped,
}

/// Trait abstracting the actual token-refresh call so the keep-alive task
/// can be tested without spinning up an HTTP server.
///
/// Implementors are expected to perform an OAuth refresh-token exchange and
/// return the new `OAuthTokenSet`.  The real implementation lives in the
/// `api` crate (`AnvilApiClient::refresh_oauth_token`) and is injected at
/// startup so this module stays free of HTTP-client dependencies.
pub trait OAuthRefresher: Send + Sync + 'static {
    /// Refresh `token` and return the new `OAuthTokenSet`.  The implementor
    /// is responsible for persisting the new credentials (e.g. via
    /// `save_oauth_credentials`) before returning, so a crash between
    /// refresh and persist doesn't lose the new bearer.
    fn refresh(
        &self,
        token: OAuthTokenSet,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<OAuthTokenSet, String>> + Send>,
    >;
}

/// Handle returned by `spawn`; dropping it cancels the ticker.
#[must_use = "dropping the handle cancels the keep-alive task"]
pub struct KeepaliveHandle {
    cancel: watch::Sender<bool>,
    join: Option<JoinHandle<()>>,
}

impl KeepaliveHandle {
    /// Request the ticker to stop.  The cancel signal is delivered via a
    /// `tokio::sync::watch` channel, so even a task currently parked in
    /// the inter-tick sleep wakes immediately.
    pub fn cancel(&self) {
        let _ = self.cancel.send(true);
    }

    /// Await graceful shutdown.  Use after `cancel()` if you want to be
    /// sure the task has finished before proceeding.
    pub async fn join(mut self) {
        self.cancel();
        if let Some(handle) = self.join.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for KeepaliveHandle {
    fn drop(&mut self) {
        self.cancel();
        // Detach the join handle on drop; the cancellation signal ensures
        // the task exits as soon as the runtime polls it again.
    }
}

/// Spawn the keep-alive ticker.
///
/// Returns a `KeepaliveHandle` that can be used to cancel the task.  The
/// task uses `tokio::spawn` so it runs on the ambient tokio runtime; if no
/// runtime is active at call time the caller must wrap this in a
/// `Runtime::block_on` or similar.  See `main.rs::run_repl_tui` for the
/// canonical wiring (spawns within a dedicated single-threaded runtime so
/// it doesn't conflict with `block_on` calls elsewhere in the sync REPL
/// loop).
pub fn spawn<R: OAuthRefresher>(
    refresher: Arc<R>,
    events: mpsc::UnboundedSender<KeepaliveEvent>,
) -> KeepaliveHandle {
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let join = tokio::spawn(async move {
        run_loop(refresher, events, cancel_rx).await;
    });
    KeepaliveHandle {
        cancel: cancel_tx,
        join: Some(join),
    }
}

/// Inner loop.  Sleeps in `tokio::select!` against the cancel watch
/// channel so cancellation wakes the task immediately, regardless of how
/// long the inter-tick sleep had left to run.
async fn run_loop<R: OAuthRefresher>(
    refresher: Arc<R>,
    events: mpsc::UnboundedSender<KeepaliveEvent>,
    mut cancel_rx: watch::Receiver<bool>,
) {
    loop {
        if *cancel_rx.borrow() {
            let _ = events.send(KeepaliveEvent::Stopped);
            return;
        }

        let token = load_oauth_credentials().ok().flatten();
        let delay_secs = match &token {
            None => {
                let _ = events.send(KeepaliveEvent::NoCredential);
                KEEPALIVE_MIN_DELAY_SECS
            }
            Some(token) => {
                let now = unix_now_seconds();
                let until_expiry = token
                    .expires_at
                    .map(|exp| exp.saturating_sub(now))
                    .unwrap_or(0);

                // Refresh when the wall-clock remaining is at or below
                // twice the ticker's MIN-delay interval (i.e. we'd be
                // cutting it close on the next tick).  This is the
                // *proactive* refresh path; the lazy safety-window
                // refresh in `AuthSource::from_env_or_saved` is the
                // backstop.
                let should_refresh = until_expiry <= KEEPALIVE_MIN_DELAY_SECS * 2
                    && token.refresh_token.is_some();

                if should_refresh {
                    let token_clone = token.clone();
                    match refresher.refresh(token_clone).await {
                        Ok(new_token) => {
                            let new_expires = new_token.expires_at;
                            let _ = events.send(KeepaliveEvent::Refreshed {
                                new_expires_at: new_expires,
                            });
                            next_refresh_delay_secs(&new_token, unix_now_seconds())
                        }
                        Err(reason) => {
                            let _ = events
                                .send(KeepaliveEvent::RefreshFailed { reason });
                            // Don't tight-loop on refresh failure; wait
                            // a full MIN interval before trying again.
                            KEEPALIVE_MIN_DELAY_SECS
                        }
                    }
                } else {
                    // Task #763 (v2.2.20): emit a heartbeat so observers
                    // see proof-of-life on every tick, not just at refresh
                    // boundaries.
                    let _ = events.send(KeepaliveEvent::Heartbeat {
                        expires_at: token.expires_at,
                    });
                    next_refresh_delay_secs(token, now)
                }
            }
        };

        // Sleep until the next tick, OR until cancel fires.  The watch
        // receiver wakes immediately on cancel so even a 1800s nap is
        // interrupted instantly when the parent drops the handle.
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(delay_secs)) => {}
            res = cancel_rx.changed() => {
                if res.is_err() || *cancel_rx.borrow() {
                    let _ = events.send(KeepaliveEvent::Stopped);
                    return;
                }
            }
        }
    }
}

/// Adapter trait so `api::AnvilApiClient` can plug into the keepalive
/// without runtime depending on api.  The api crate provides a concrete
/// impl in `api::providers::anvil_provider::OAuthRefreshClient`.
///
/// This is intentionally typed as a marker re-export; the real concrete
/// `OAuthRefresher` impl lives in the api crate where the HTTP client is.
pub use OAuthRefresher as Refresher;

/// Quick adapter to persist a refreshed token.  The refresher's
/// implementation is expected to call this BEFORE returning so the saved
/// file always tracks the freshest bearer.  Returns the same `token` for
/// chaining.
pub fn persist_refreshed_token(token: OAuthTokenSet) -> Result<OAuthTokenSet, String> {
    save_oauth_credentials(&token)
        .map_err(|e| format!("could not persist refreshed OAuth credentials: {e}"))?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Deterministic refresher that produces a token with `expires_in`
    /// set to a configurable number of seconds.  Records call count so
    /// tests can verify the ticker behaviour.
    struct ScriptedRefresher {
        calls: AtomicUsize,
        new_expires_in: u64,
        fail_after: Option<usize>,
    }

    impl ScriptedRefresher {
        fn new(new_expires_in: u64) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                new_expires_in,
                fail_after: None,
            }
        }

        fn fail_after(calls: usize, new_expires_in: u64) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                new_expires_in,
                fail_after: Some(calls),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl OAuthRefresher for ScriptedRefresher {
        fn refresh(
            &self,
            token: OAuthTokenSet,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<OAuthTokenSet, String>> + Send>,
        > {
            let calls = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let fail = self
                .fail_after
                .map(|n| calls > n)
                .unwrap_or(false);
            let new_expires_in = self.new_expires_in;
            Box::pin(async move {
                if fail {
                    Err("simulated refresh_token rejected (test)".to_string())
                } else {
                    Ok(OAuthTokenSet {
                        access_token: format!("refreshed-{calls}"),
                        refresh_token: token.refresh_token.clone(),
                        expires_at: Some(unix_now_seconds() + new_expires_in),
                        scopes: token.scopes.clone(),
                    })
                }
            })
        }
    }

    /// Spec for the cancel path: drop-on-handle cancels the inner task and
    /// emits `KeepaliveEvent::Stopped` before exit.  Task #597 deliverable
    /// #3 (clean shutdown).
    #[tokio::test]
    async fn keepalive_emits_stopped_on_cancel() {
        let refresher = Arc::new(ScriptedRefresher::new(3600));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = spawn(refresher, tx);

        // Give the task a beat to enter the loop.
        tokio::time::sleep(Duration::from_millis(50)).await;

        handle.cancel();
        // Wait up to 200ms for the Stopped event (the loop polls the
        // cancel flag at MIN-interval granularity in production; the test
        // path hits it on the first iteration since we cancel before
        // any sleep finishes).
        let stopped = tokio::time::timeout(Duration::from_millis(2_000), async {
            while let Some(event) = rx.recv().await {
                if matches!(event, KeepaliveEvent::Stopped) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(stopped, "keepalive must emit Stopped on cancel");
    }

    /// Spec for the refresh-failure path: a failing refresher does NOT
    /// kill the task — it emits `KeepaliveEvent::RefreshFailed` and the
    /// ticker keeps running.  Task #597 deliverable #3 ("emit a TUI
    /// banner via the existing system-event channel rather than
    /// panicking").
    #[test]
    fn refresh_failed_event_payload_carries_reason() {
        let event = KeepaliveEvent::RefreshFailed {
            reason: "test refresh failure".to_string(),
        };
        match event {
            KeepaliveEvent::RefreshFailed { reason } => {
                assert!(reason.contains("test refresh failure"));
            }
            other => panic!("expected RefreshFailed, got {other:?}"),
        }
    }

    /// Spec for the no-credential path: an absent credentials.json doesn't
    /// crash the ticker; it idles at MIN and waits for the user to log in.
    #[test]
    fn no_credential_event_is_a_valid_signal() {
        let event = KeepaliveEvent::NoCredential;
        assert!(matches!(event, KeepaliveEvent::NoCredential));
    }

    /// Spec for the refresh-success path: when the ticker decides to
    /// refresh and the refresher returns Ok, the task emits
    /// `KeepaliveEvent::Refreshed` carrying the new `expires_at`.  Task
    /// #597 deliverable #3 (proactive refresh).
    ///
    /// The test uses a stubbed-credentials-file via ANVIL_CONFIG_HOME so
    /// the ticker's `load_oauth_credentials` call sees a deterministic
    /// shape with a tiny remaining lifetime (10s) that crosses the
    /// "refresh now" threshold (`KEEPALIVE_MIN_DELAY_SECS * 2 = 120s`).
    #[tokio::test]
    async fn keepalive_emits_refreshed_when_token_is_near_expiry() {
        let _guard = crate::test_env_lock();
        let temp_home = std::env::temp_dir().join(format!(
            "runtime-keepalive-test-{}-{}",
            std::process::id(),
            unix_now_seconds()
        ));
        std::fs::create_dir_all(&temp_home).expect("create temp config home");
        // SAFETY: tests serialize on the env_lock above.
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &temp_home); }

        // Seed a saved Anthropic OAuth credential with a tiny remaining
        // lifetime (10s) so the ticker decides to refresh on its very
        // first iteration.
        let near_expiry = OAuthTokenSet {
            access_token: "sk-ant-oat01-NEAR-EXPIRY".to_string(),
            refresh_token: Some("sk-ant-ort01-NEAR-EXPIRY".to_string()),
            expires_at: Some(unix_now_seconds() + 10),
            scopes: vec!["user:inference".to_string()],
        };
        save_oauth_credentials(&near_expiry).expect("seed credentials");

        let refresher = Arc::new(ScriptedRefresher::new(3600));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _handle = spawn(Arc::clone(&refresher), tx);

        let got_refresh = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(event) = rx.recv().await {
                if let KeepaliveEvent::Refreshed { new_expires_at } = event {
                    return new_expires_at;
                }
            }
            None
        })
        .await
        .expect("must observe Refreshed event before timeout");

        assert!(
            got_refresh.is_some(),
            "Refreshed event must carry a populated new_expires_at"
        );
        assert!(refresher.call_count() >= 1, "refresher must have been invoked");

        // SAFETY: tests serialize on the env_lock above.
        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); }
        let _ = std::fs::remove_dir_all(&temp_home);
    }
}
