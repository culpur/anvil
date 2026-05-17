//! Background-task handlers spawned at TUI startup.
//!
//! Lives outside `main.rs` per the modularity rule
//! (`feedback-anvil-main-rs-modularity.md`): each long-running task gets
//! its own `spawn_*` function in this module, returning an `Arc<Mutex<...>>`
//! that the TUI main loop polls with `try_lock()` per frame.
//!
//! Currently hosts:
//!   - `spawn_update_check` — once-on-startup probe + 24h-cached GitHub
//!     Releases lookup. Replaces the inline `thread::spawn` block that used
//!     to live in `main.rs`.
//!   - `spawn_qmd_poll` — 30s-tick refresh of QMD index status so the rail's
//!     MEMORY section shows live archive counts.
//!   - `spawn_oauth_keepalive` — background ticker that proactively refreshes
//!     the Anthropic OAuth bearer before expiry (Task #597, three-layer
//!     credential keeper: safety window + 401 retry + keep-alive ticker).

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use runtime::qmd::QmdStatus;
use runtime::update_check;

/// Slot type used for both `update_check` and `qmd_poll` results.
///
/// `Option::Some` means "fresh value to consume"; the main loop calls
/// `slot.take()` after consuming so the next frame sees `None` until the
/// background thread writes again.
pub type Slot<T> = Arc<Mutex<Option<T>>>;

/// Spawn the once-at-startup GitHub Releases probe.
///
/// Returns a slot that the main loop consumes on each frame. When the
/// returned message becomes available, the TUI's `set_update_available`
/// renders it on the rail above the BUILD line.
///
/// Endpoint: `https://api.github.com/repos/culpur/anvil/releases/latest`
/// via `runtime::update_check::check`. The on-disk cache at
/// `~/.anvil/update_check.json` throttles the actual network probe to once
/// per 24h regardless of how many TUI launches happen, and can be primed
/// manually for testing.
pub fn spawn_update_check(current_version: String) -> Slot<String> {
    let slot: Slot<String> = Arc::new(Mutex::new(None));
    let writer = Arc::clone(&slot);
    thread::spawn(move || {
        if let Some(latest) = update_check::check(&current_version) {
            let msg = format!(
                "Update available! {current_version} → {latest}  Run: anvil --update"
            );
            if let Ok(mut s) = writer.lock() {
                *s = Some(msg);
            }
        }
    });
    slot
}

/// Handle returned by `spawn_oauth_keepalive`.  Holds the slot that the
/// main loop polls for `KeepaliveEvent` messages.  The dedicated thread
/// running the tokio runtime is detached; it exits when the underlying
/// runtime task finishes (i.e. when the `KeepaliveHandle` inside the
/// runtime is dropped at process exit).
pub struct KeepaliveBg {
    /// Most-recent event from the ticker.  `Option::Some` means "fresh
    /// value to consume"; the main loop calls `take()` after pushing the
    /// banner so the next frame sees `None` until the ticker writes again.
    pub last_event: Slot<runtime::KeepaliveEvent>,
}

/// Spawn the OAuth keep-alive ticker (Task #597 deliverable #3 / #4).
///
/// The ticker proactively refreshes the saved Anthropic OAuth bearer
/// before it expires so an idle user does not hit a forced re-OAuth.
/// On refresh failure (refresh_token rejected, network down) it emits a
/// `KeepaliveEvent::RefreshFailed` which the main loop surfaces as a
/// TUI banner — the task itself does not panic or exit.
///
/// Implementation note: `main.rs` is synchronous, so the ticker runs in
/// its own thread with a dedicated single-threaded tokio runtime.  The
/// runtime's `block_on` parks the thread until the ticker exits naturally
/// (cancellation flag set), which happens when the parent
/// `runtime::KeepaliveHandle` is dropped via the slot owner going out of
/// scope at REPL shutdown.  Errors from the tokio runtime constructor
/// (rare; only fails on resource exhaustion) leave the slot permanently
/// empty — the lazy 401-retry path is still active so OAuth still works.
#[must_use]
pub fn spawn_oauth_keepalive() -> KeepaliveBg {
    let slot: Slot<runtime::KeepaliveEvent> = Arc::new(Mutex::new(None));
    let writer = Arc::clone(&slot);
    thread::spawn(move || {
        let runtime_handle = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(_) => return,
        };
        runtime_handle.block_on(async move {
            let refresher = Arc::new(api::AnthropicKeepaliveRefresher);
            let (tx, mut rx) =
                tokio::sync::mpsc::unbounded_channel::<runtime::KeepaliveEvent>();
            let _handle = runtime::spawn_oauth_keepalive(refresher, tx);
            while let Some(event) = rx.recv().await {
                let is_terminal = matches!(event, runtime::KeepaliveEvent::Stopped);
                if let Ok(mut s) = writer.lock() {
                    *s = Some(event);
                }
                if is_terminal {
                    break;
                }
            }
        });
    });
    KeepaliveBg { last_event: slot }
}

/// Spawn the 30-second QMD status poller.
///
/// Each tick re-runs `qmd status` on the worker thread and writes the
/// parsed `QmdStatus` to the slot. The TUI main loop's per-frame
/// `try_lock` consumes the value and forwards it to
/// `AnvilTui::set_qmd_status`. When QMD is not installed the worker exits
/// immediately and the slot stays empty for the session.
///
/// Quiet failures: any error from `qmd status` simply leaves the previous
/// value in place; the rail keeps showing the last successful read.
pub fn spawn_qmd_poll() -> Slot<QmdStatus> {
    let slot: Slot<QmdStatus> = Arc::new(Mutex::new(None));
    let writer = Arc::clone(&slot);
    thread::spawn(move || {
        // Build a fresh client inside the thread so we don't share state
        // with the foreground `cli.qmd` instance.
        let client = runtime::qmd::QmdClient::new();
        if !client.is_enabled() {
            return;
        }
        loop {
            if let Some(status) = client.status() {
                if let Ok(mut s) = writer.lock() {
                    *s = Some(status);
                }
            }
            thread::sleep(Duration::from_secs(30));
        }
    });
    slot
}
