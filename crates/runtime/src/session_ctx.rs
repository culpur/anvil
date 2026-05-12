//! Thread-local per-session context for ambient child-process env injection.
//!
//! **Problem.** Anvil spawns child processes (Bash tool, hooks, MCP stdio
//! servers) that should inherit per-session identity: which session ID is
//! running, what effort level the user has chosen, what project directory
//! the session is rooted in. Threading these through every tool dispatch
//! signature (`execute_tool`, `execute_bash`, `run_command`) would require
//! changing public APIs in three crates and dozens of call sites.
//!
//! **Why thread-local.** Per-tab parallel inference (v2.2.12 #433) runs
//! each turn on its own OS thread via `thread::spawn`. A thread-local
//! `SessionContext` partitions correctly across tabs: tab 1's spawn sees
//! tab 1's session ID, tab 2's spawn sees tab 2's. A process-wide env or
//! a `static Mutex<...>` would conflate the two and silently mislead any
//! user script that read `$ANVIL_SESSION_ID`.
//!
//! **Async note.** Tokio tasks may migrate across worker threads at
//! `.await` points. This module is for **OS-thread-owned** call sites
//! (Bash subprocess spawn, hook subprocess spawn, MCP stdio spawn). All
//! current consumers run inside `std::thread::spawn`-rooted call trees;
//! the tokio runtime used inside `execute_bash` is single-threaded
//! (`Builder::new_current_thread`) so the thread-local is stable for
//! the lifetime of that runtime.
//!
//! **Mutation discipline.** `set()` overwrites the entire context as one
//! atomic field write (the inner `Option` swap). Don't try to mutate
//! pieces individually — set the full context once at tool dispatch
//! entry, read it in the child-spawn site, and let it persist for the
//! duration of the turn on that thread.
//!
//! ## Usage
//!
//! ```ignore
//! // Conversation runtime, before dispatching a tool call:
//! session_ctx::set(SessionContext {
//!     session_id: "01HX...".to_string(),
//!     effort_level: "high".to_string(),
//!     project_dir: PathBuf::from("/Users/me/project"),
//! });
//!
//! // bash.rs / hooks.rs, before spawning a child:
//! if let Some(ctx) = session_ctx::get() {
//!     cmd.env("ANVIL_SESSION_ID", &ctx.session_id);
//!     cmd.env("ANVIL_EFFORT", &ctx.effort_level);
//!     cmd.env("ANVIL_PROJECT_DIR", ctx.project_dir.as_os_str());
//! }
//! ```

use std::cell::RefCell;
use std::path::PathBuf;

/// A snapshot of per-session context for child-process env injection.
///
/// All fields are owned (no borrows) so the snapshot is cheap to clone
/// and outlives the parent stack frame. Cloning is the normal access
/// pattern — call sites get an `Option<SessionContext>` from `get()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionContext {
    /// Stable session identifier (UUID or session-meta ULID).
    /// Matches CC's `CLAUDE_CODE_SESSION_ID` (v2.1.132).
    pub session_id: String,
    /// Effort level string: "minimal" | "low" | "medium" | "high".
    /// Matches CC's `CLAUDE_EFFORT` (v2.1.133).
    pub effort_level: String,
    /// Absolute path to the project directory (typically the cwd at
    /// anvil launch). Matches CC's `CLAUDE_PROJECT_DIR` (v2.1.139).
    pub project_dir: PathBuf,
}

thread_local! {
    static CURRENT: RefCell<Option<SessionContext>> = const { RefCell::new(None) };
}

/// Install `ctx` as the current thread's session context.
///
/// Replaces any prior context on this thread. Typically called by the
/// conversation runtime immediately before tool dispatch.
pub fn set(ctx: SessionContext) {
    CURRENT.with(|cell| {
        *cell.borrow_mut() = Some(ctx);
    });
}

/// Clear the current thread's session context.
///
/// Optional — the thread-local is naturally dropped when the OS thread
/// exits. Call this only when you want subsequent child spawns on the
/// same thread to inherit no session env (e.g., during inline operations
/// that shouldn't leak session identity into unrelated subprocesses).
pub fn clear() {
    CURRENT.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

/// Snapshot the current thread's session context, if any.
///
/// Returns a clone — call sites can hold it across child-process
/// spawn without retaining the thread-local borrow.
#[must_use]
pub fn get() -> Option<SessionContext> {
    CURRENT.with(|cell| cell.borrow().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SessionContext {
        SessionContext {
            session_id: "test-session-id".to_string(),
            effort_level: "medium".to_string(),
            project_dir: PathBuf::from("/tmp/proj"),
        }
    }

    #[test]
    fn get_returns_none_when_unset() {
        // Run on a fresh thread to avoid leakage from other tests.
        let result = std::thread::spawn(|| get()).join().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn set_then_get_round_trips() {
        std::thread::spawn(|| {
            let ctx = sample();
            set(ctx.clone());
            assert_eq!(get(), Some(ctx));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn clear_drops_context() {
        std::thread::spawn(|| {
            set(sample());
            assert!(get().is_some());
            clear();
            assert!(get().is_none());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_overwrites_prior_context() {
        std::thread::spawn(|| {
            set(sample());
            let mut updated = sample();
            updated.effort_level = "high".to_string();
            set(updated.clone());
            assert_eq!(get().unwrap().effort_level, "high");
        })
        .join()
        .unwrap();
    }

    #[test]
    fn context_is_per_thread() {
        // Set on thread A, verify thread B sees None, then thread A
        // still sees its value.
        let handle_a = std::thread::spawn(|| {
            set(sample());
            // Spawn a sibling thread and assert it sees nothing.
            let inner = std::thread::spawn(|| get());
            let inner_result = inner.join().unwrap();
            assert!(
                inner_result.is_none(),
                "child thread should not inherit parent's session ctx"
            );
            // Thread A still has its value.
            assert_eq!(get(), Some(sample()));
        });
        handle_a.join().unwrap();
    }
}
