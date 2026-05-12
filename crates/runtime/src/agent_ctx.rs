//! Process-scoped subagent identity, attached to every OTel event.
//!
//! CC-139-F16 parity. When a subagent (TeamDelegate, plugin-spawned
//! worker, etc.) is running, OTel events emitted from its codepath
//! carry `agent_id` and `parent_agent_id` headers so traces can be
//! reassembled across the parent ↔ child boundary.
//!
//! Design intent:
//!   - Stack of contexts so nested subagents (parent → child → grandchild)
//!     restore the parent's context correctly on completion.
//!   - Frame stores both the child's own id and its parent id, so a
//!     single snapshot answers both questions without walking the stack.
//!   - `with_current` reads the top frame atomically without surfacing
//!     the Mutex to callers; that keeps emit_event lock-only-on-emit.
//!   - Always best-effort: a poisoned mutex returns None rather than
//!     panicking, so a bug in trace-attaching can't kill a real tool
//!     call.

use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentContext {
    pub agent_id: String,
    pub parent_agent_id: Option<String>,
}

static STACK: OnceLock<Mutex<Vec<AgentContext>>> = OnceLock::new();

fn stack() -> &'static Mutex<Vec<AgentContext>> {
    STACK.get_or_init(|| Mutex::new(Vec::new()))
}

/// Push a new subagent context on top of the stack. The caller is
/// responsible for matching this with a [`pop`] when the subagent
/// completes; the typical pattern is to use [`PushGuard`] which calls
/// pop on Drop.
pub fn push(ctx: AgentContext) {
    if let Ok(mut guard) = stack().lock() {
        guard.push(ctx);
    }
}

/// Pop the top context. Returns `None` if the stack was empty or the
/// mutex was poisoned.
pub fn pop() -> Option<AgentContext> {
    stack().lock().ok().and_then(|mut g| g.pop())
}

/// Read the current top-of-stack context without mutating. Cloned for
/// caller use.
#[must_use]
pub fn current() -> Option<AgentContext> {
    stack().lock().ok().and_then(|g| g.last().cloned())
}

/// Run `f` with a read-only borrow of the current context. Avoids the
/// clone in `current()` for hot paths like emit_event attribute build.
pub fn with_current<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&AgentContext) -> R,
{
    let guard = stack().lock().ok()?;
    guard.last().map(f)
}

/// RAII guard. Push the context on construction, pop on drop.
///
/// Typical use:
/// ```ignore
/// let _g = agent_ctx::PushGuard::new(AgentContext { ... });
/// // ... subagent runs here ...
/// // pop happens automatically when _g goes out of scope
/// ```
pub struct PushGuard {
    /// Tracks whether this guard is responsible for popping. A poisoned
    /// stack mutex on push means we *didn't* successfully push, so we
    /// also shouldn't pop on drop.
    pushed: bool,
}

impl PushGuard {
    #[must_use]
    pub fn new(ctx: AgentContext) -> Self {
        let pushed = stack()
            .lock()
            .map(|mut g| {
                g.push(ctx);
                true
            })
            .unwrap_or(false);
        Self { pushed }
    }
}

impl Drop for PushGuard {
    fn drop(&mut self) {
        if self.pushed {
            let _ = pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize all tests in this module — the stack is process-wide,
    /// so parallel tests racing on it would cause spurious failures.
    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn reset() {
        if let Ok(mut g) = stack().lock() {
            g.clear();
        }
    }

    #[test]
    fn current_is_none_when_stack_empty() {
        let _g = test_lock();
        reset();
        assert!(current().is_none());
    }

    #[test]
    fn push_then_current_returns_pushed_value() {
        let _g = test_lock();
        reset();
        let ctx = AgentContext {
            agent_id: "child-1".to_string(),
            parent_agent_id: Some("parent".to_string()),
        };
        push(ctx.clone());
        assert_eq!(current(), Some(ctx));
        reset();
    }

    #[test]
    fn nested_push_preserves_parent_on_pop() {
        let _g = test_lock();
        reset();
        push(AgentContext {
            agent_id: "parent".to_string(),
            parent_agent_id: None,
        });
        push(AgentContext {
            agent_id: "child".to_string(),
            parent_agent_id: Some("parent".to_string()),
        });
        assert_eq!(current().unwrap().agent_id, "child");
        let popped = pop().unwrap();
        assert_eq!(popped.agent_id, "child");
        assert_eq!(current().unwrap().agent_id, "parent");
        reset();
    }

    #[test]
    fn push_guard_pops_on_drop() {
        let _g = test_lock();
        reset();
        {
            let _pg = PushGuard::new(AgentContext {
                agent_id: "scoped".to_string(),
                parent_agent_id: None,
            });
            assert_eq!(current().unwrap().agent_id, "scoped");
        }
        assert!(current().is_none(), "guard should have popped");
    }

    #[test]
    fn with_current_returns_none_when_empty() {
        let _g = test_lock();
        reset();
        let result: Option<usize> = with_current(|c| c.agent_id.len());
        assert!(result.is_none());
    }

    #[test]
    fn with_current_returns_some_when_pushed() {
        let _g = test_lock();
        reset();
        push(AgentContext {
            agent_id: "abc".to_string(),
            parent_agent_id: None,
        });
        let len = with_current(|c| c.agent_id.len());
        assert_eq!(len, Some(3));
        reset();
    }
}
