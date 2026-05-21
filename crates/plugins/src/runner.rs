//! Skill runner — re-entry guard for fork-context invocations.
//!
//! Task #720 / CC parity v2.1.145-B4. Audit doc:
//! docs/cc-parity-audit-2026-05-21.md TASK-J.
//!
//! When a skill is invoked with `context: fork` (spawning a forked
//! subagent context), it must not be able to invoke itself again while
//! the original invocation is still on the stack — otherwise a self-
//! referencing skill produces an infinite loop that consumes the user's
//! token budget and never terminates.
//!
//! This module is the canonical home of the guard. It tracks active
//! fork-context invocations per (session, skill) and refuses re-entry
//! when an inner invocation tries to re-enter the same skill on the
//! same session. The guard is a RAII handle; the entry is popped on
//! Drop so panicking skill code can't poison the table.
//!
//! ## Why a separate module
//!
//! Anvil's `context: fork` semantics will be wired into skill manifests
//! in a later phase. The guard is added now in anticipation of that
//! wiring so the security invariant is in place before any user-facing
//! fork-context surface ships. Future callers should:
//!
//! 1. Build the `ForkContextRegistry` once per session container.
//! 2. Acquire a `ForkGuard` before invoking the skill body.
//! 3. Release (drop) the guard when the body returns.
//!
//! When the guard cannot be acquired (recursive invocation detected),
//! callers must abort with the spec-mandated error string
//! `Skill 'X' attempted recursive fork-context invocation`.
//!
//! Implementation notes
//! - `std::sync::Mutex` is used over `parking_lot` to stay within the
//!   plugins crate's existing dependency set.
//! - `SessionId` and `SkillName` are owned `String`s for ergonomic
//!   sharing across threads; lookups use `&str`.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Identifier of a host session under which skills run.
pub type SessionId = String;
/// Identifier of a skill (the skill's manifest `name`).
pub type SkillName = String;

/// Process-shared registry of active fork-context invocations.
///
/// Holds, per session, the set of skill names whose fork-context bodies
/// are currently on the stack. Lookups are O(1); concurrent acquires
/// across sessions don't contend (one Mutex over the outer map; a
/// future optimisation can shard if contention is measured).
#[derive(Debug, Default, Clone)]
pub struct ForkContextRegistry {
    inner: Arc<Mutex<HashMap<SessionId, HashSet<SkillName>>>>,
}

impl ForkContextRegistry {
    /// Construct an empty registry. Cheap; cloning is also cheap (Arc).
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Try to acquire a fork-context guard for `(session, skill)`.
    ///
    /// Returns `Ok(ForkGuard)` if the skill is not currently active on
    /// this session — the entry is pushed onto the active set and will
    /// pop when the guard is dropped.
    ///
    /// Returns `Err(RecursiveForkInvocation { skill })` if the same skill
    /// is already on the stack for the same session. The caller MUST
    /// abort the invocation with the spec error message.
    ///
    /// If the registry mutex is poisoned (some other thread panicked
    /// while holding it), we recover the inner map — a panicked acquire
    /// must not poison every subsequent guard request.
    pub fn try_acquire(
        &self,
        session_id: &str,
        skill_name: &str,
    ) -> Result<ForkGuard, RecursiveForkInvocation> {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let entry = guard
            .entry(session_id.to_string())
            .or_insert_with(HashSet::new);
        if entry.contains(skill_name) {
            return Err(RecursiveForkInvocation {
                skill: skill_name.to_string(),
            });
        }
        entry.insert(skill_name.to_string());
        Ok(ForkGuard {
            registry: self.inner.clone(),
            session_id: session_id.to_string(),
            skill_name: skill_name.to_string(),
            released: false,
        })
    }

    /// Returns `true` if `(session, skill)` is currently on the active
    /// stack. Provided for tests and diagnostics; production callers
    /// should always use [`try_acquire`].
    #[must_use]
    pub fn is_active(&self, session_id: &str, skill_name: &str) -> bool {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard
            .get(session_id)
            .is_some_and(|set| set.contains(skill_name))
    }
}

/// RAII handle that pops the active entry on Drop.
///
/// Hold this for the duration of the fork-context skill body. Dropping
/// the guard (normal return, `?`, panic unwind) releases the slot so a
/// future invocation of the same skill on the same session is allowed.
#[derive(Debug)]
pub struct ForkGuard {
    registry: Arc<Mutex<HashMap<SessionId, HashSet<SkillName>>>>,
    session_id: String,
    skill_name: String,
    released: bool,
}

impl Drop for ForkGuard {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let mut guard = match self.registry.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(set) = guard.get_mut(&self.session_id) {
            set.remove(&self.skill_name);
            if set.is_empty() {
                guard.remove(&self.session_id);
            }
        }
        self.released = true;
    }
}

/// Error returned by [`ForkContextRegistry::try_acquire`] when a skill
/// re-enters itself in fork-context mode on the same session.
///
/// The `Display` impl matches the spec-mandated message exactly so
/// downstream callers can propagate it verbatim:
///
/// ```text
/// Skill 'X' attempted recursive fork-context invocation
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecursiveForkInvocation {
    /// Name of the skill that attempted re-entry.
    pub skill: String,
}

impl std::fmt::Display for RecursiveForkInvocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Skill '{}' attempted recursive fork-context invocation",
            self.skill
        )
    }
}

impl std::error::Error for RecursiveForkInvocation {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_acquire_succeeds() {
        let reg = ForkContextRegistry::new();
        let g = reg.try_acquire("session-1", "audit");
        assert!(g.is_ok(), "first acquire must succeed");
        assert!(reg.is_active("session-1", "audit"));
    }

    #[test]
    fn second_acquire_on_same_session_and_skill_is_recursive() {
        // The TASK-J regression test: a skill re-invokes itself via
        // context: fork. The second acquire must error.
        let reg = ForkContextRegistry::new();
        let _outer = reg.try_acquire("session-1", "audit").expect("outer ok");
        let inner = reg.try_acquire("session-1", "audit");
        let Err(err) = inner else {
            panic!("second acquire must be Err");
        };
        assert_eq!(err.skill, "audit");
        assert!(
            err.to_string().contains("recursive fork-context"),
            "error message must match the spec: {err}"
        );
        assert_eq!(
            err.to_string(),
            "Skill 'audit' attempted recursive fork-context invocation",
            "verbatim spec message"
        );
    }

    #[test]
    fn different_skill_on_same_session_is_allowed() {
        let reg = ForkContextRegistry::new();
        let _g1 = reg.try_acquire("session-1", "audit").expect("ok");
        let g2 = reg.try_acquire("session-1", "lint");
        assert!(g2.is_ok(), "different skill must not collide");
    }

    #[test]
    fn same_skill_on_different_session_is_allowed() {
        // The (session_id, skill_name) tuple is the cycle key. Two
        // independent sessions running the same skill concurrently is
        // fine — neither cycles into itself.
        let reg = ForkContextRegistry::new();
        let _g1 = reg.try_acquire("session-1", "audit").expect("ok");
        let g2 = reg.try_acquire("session-2", "audit");
        assert!(g2.is_ok(), "same skill on different sessions must not collide");
    }

    #[test]
    fn drop_releases_the_slot() {
        let reg = ForkContextRegistry::new();
        {
            let _g = reg.try_acquire("session-1", "audit").expect("ok");
            assert!(reg.is_active("session-1", "audit"));
        }
        // After the outer scope, the entry must be gone.
        assert!(!reg.is_active("session-1", "audit"));
        // And re-acquire must succeed.
        let g2 = reg.try_acquire("session-1", "audit");
        assert!(g2.is_ok(), "drop must release the slot");
    }

    #[test]
    fn nested_then_release_then_reacquire() {
        // Acquire outer, attempt inner (must fail with recursive),
        // outer drops normally, then a sibling can acquire again.
        let reg = ForkContextRegistry::new();
        let outer = reg.try_acquire("s", "k").expect("outer ok");

        // Inner re-entry must fail with the recursive error.
        let inner = reg.try_acquire("s", "k");
        assert!(inner.is_err(), "inner re-entry must fail");

        drop(outer);
        assert!(!reg.is_active("s", "k"));

        let again = reg.try_acquire("s", "k");
        assert!(again.is_ok(), "after release, sibling acquire ok");
    }

    #[test]
    fn registry_shares_state_across_clones() {
        // Cloning the registry yields a new Arc handle pointing at the
        // same inner map. A guard acquired on clone-A is visible to
        // clone-B.
        let reg_a = ForkContextRegistry::new();
        let reg_b = reg_a.clone();
        let _g = reg_a.try_acquire("s", "k").expect("ok");
        assert!(
            reg_b.is_active("s", "k"),
            "clones must share state via Arc"
        );
        let collision = reg_b.try_acquire("s", "k");
        assert!(
            collision.is_err(),
            "cross-clone recursion must be detected"
        );
    }

    /// Simulates the actual skill-runner pattern: a mock "skill" that
    /// recursively calls itself via the registry. The first level
    /// succeeds; the inner call returns the recursive error.
    #[test]
    fn mock_skill_recursive_fork_invocation_aborts_with_spec_error() {
        let reg = ForkContextRegistry::new();

        fn run_skill(
            reg: &ForkContextRegistry,
            session: &str,
            name: &str,
            depth: u32,
        ) -> Result<u32, RecursiveForkInvocation> {
            let _guard = reg.try_acquire(session, name)?;
            if depth == 0 {
                return Ok(0);
            }
            // Recursive self-invocation in fork-context — must fail.
            let inner = run_skill(reg, session, name, depth - 1)?;
            Ok(inner + 1)
        }

        let outcome = run_skill(&reg, "session-1", "self-loop", 3);
        let Err(err) = outcome else {
            panic!("recursive skill must abort, got Ok");
        };
        assert_eq!(
            err.to_string(),
            "Skill 'self-loop' attempted recursive fork-context invocation"
        );

        // After failure, the registry must be empty — the outermost
        // guard drops on the way out and clears the entry.
        assert!(!reg.is_active("session-1", "self-loop"));
    }
}
