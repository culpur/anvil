# Permission Chain — Decision Order Documentation

## Overview

Every tool call in Anvil passes through a sequential permission chain before the
tool is executed or denied.  The chain is implemented in
`crates/runtime/src/conversation/permission_gate.rs::evaluate_and_execute`.

This document pins the exact evaluation order, cites the code location for each
step, explains the precedence rule, and gives a concrete example of when each
step fires.

---

## Decision Order (authoritative)

```
(1) Auto-mode hard-deny
(2) PreToolUse hook(s) emit deny/allow/prompt
(3) PermissionMemory Deny
(4) Reviewer agent (if enabled)
(5) PermissionMemory Allow
(6) Policy + prompter
```

The first step that produces a `Deny` terminates the chain immediately.  The
first step that produces an `Allow` (or shortcircuit) also terminates without
consulting later steps — **except** that the PreToolUse hook chain still runs
for memory-Allow shortcircuits (see Step 5 note below).

---

## Step-by-step

### (1) Auto-mode hard-deny

**File:** `crates/runtime/src/conversation/permission_gate.rs:55-69`

**Rule:**  When `PermissionMode::WorkspaceWrite` is active and the tool name (or
its input) matches a pattern in `AutoModeConfig::hard_deny`, the call is refused
unconditionally.  No other step is consulted — not hooks, not memory, not the
reviewer.  This is the user's unbypassable veto for dangerous operations while
still using auto-mode for routine work.

`ReadOnly` and `DangerFullAccess` are exempt: `ReadOnly` already blocks via the
policy gate, and `DangerFullAccess` opts out of all guardrails.

**Fires when:** The user has added e.g. `"Bash(rm -rf *)"` to
`autoMode.hard_deny` in `settings.json` and a bash call whose input contains
`rm -rf` arrives.

**Skips:** All subsequent steps.

---

### (2) PermissionRequest hook(s)

**File:** `crates/runtime/src/conversation/permission_gate.rs:95-99`
**Hook runner:** `crates/runtime/src/hooks.rs::HookRunner::run_permission_request`

**Rule:**  The `PreToolUse`/`PermissionRequest` hook chain runs *before* the
policy gate's final check.  This is intentional — hooks exist to let plugins
customise permission flow.  A hook may emit a `permissionDecision` of
`allow | deny | ask | defer` in its JSON stdout (or exit code 2 for deny).

- `Deny` from any hook short-circuits the chain; no further steps run.
- `Allow` from any hook bypasses PermissionMemory Deny, Reviewer, and
  PermissionMemory Allow — it jumps directly to `run_allow_branch`.
- `Ask` / `Defer` / no decision → fall through to Step 3.

**Note:** The hook chain fires BEFORE the PermissionMemory Deny check (Step 3)
because hooks represent plugin-level authority that should be able to override
even user-recorded memory vetoes.  A plugin acting as an auditor can force
a deny that the user's session memory could not have anticipated.

**Fires when:** A plugin ships a `PermissionRequest` hook (e.g. a credential
scanner that blocks calls containing `SECRET=` in the input), or a user adds a
shell hook to `settings.json` that returns a JSON decision.

---

### (3) PermissionMemory Deny

**File:** `crates/runtime/src/conversation/permission_gate.rs:108-122`
**PermissionMemory:** `crates/runtime/src/permission_memory.rs`

**Rule:**  If the `PermissionMemory` store holds a `Deny` effect for this tool
(optionally scoped to an input pattern), the call is denied UNLESS a hook has
already emitted `Allow` in Step 2 (a hook Allow overrides even a memory veto,
because the hook has made a deliberate runtime decision).

**Fires when:** The user previously selected "deny always" for a tool in the
interactive prompter, recording a `Deny` grant.  On subsequent calls the gate
short-circuits here without prompting.

---

### (4) Reviewer agent

**File:** `crates/runtime/src/conversation/permission_gate.rs:150-236`
**Reviewer:** `crates/runtime/src/permissions/reviewer.rs`

**Rule:**  The reviewer is a deterministic, synchronous scanner — no LLM, no
network.  It fires after the hook chain (so hook `Allow` decisions bypass it)
and before the policy prompter.

- `Recommendation::Deny` → auto-deny without prompting.
- `Recommendation::Warn` → the prompter is wrapped with an `AnnotatingPrompter`
  so the warning appears in the user's approval dialog.
- `Recommendation::Allow` → no match; fall through to Step 5.

**Fires when:** A pattern in `ReviewerConfig` (e.g. a blocklist of dangerous
filesystem paths) matches the tool input.

---

### (5) PermissionMemory Allow

**File:** `crates/runtime/src/conversation/permission_gate.rs:126-136`

**Rule:**  If the memory store holds an `Allow` effect for this tool, the call
is permitted and the executor is invoked via `run_allow_branch` — UNLESS a hook
emitted `Deny` in Step 2 (hook Deny outranks a memory Allow).

**Important:** `run_allow_branch` still fires the `PreToolUse` hook chain
(see `crates/runtime/src/conversation/permission_gate.rs:281-298`).  Memory
bypasses the *prompt*, not the hook safety net.

**Fires when:** The user previously selected "allow always" for a tool in the
prompter.  On subsequent calls the prompter is skipped but hooks still run.

---

### (6) Policy + prompter

**File:** `crates/runtime/src/conversation/permission_gate.rs:168-238`

**Rule:**  Normal policy evaluation: `PermissionPolicy::authorize` checks the
required `PermissionMode` for the tool against the active mode.  If the mode is
insufficient, the call is denied without prompting (no prompter present) or the
user is asked (prompter present).

A `PersistingPrompter` wrapper (line 370-391) records `AllowAlways` decisions
back into `PermissionMemory` so Step 5 can short-circuit future calls.

**Fires when:** No hook, no memory record, and no reviewer match applies.  This
is the common case for new tools in a new session.

---

## Decision-order test

The ordering is pinned by the existing tests in
`crates/runtime/src/conversation/permission_gate.rs` (starting at the `#[cfg(test)]`
block):

| Scenario | Key assertion |
|---|---|
| `hard_deny_blocks_in_workspace_write_mode` | Step 1 fires before hooks |
| `hard_deny_skipped_in_read_only_mode` | Step 1 exempt in ReadOnly |
| `memory_deny_effect_outranks_reviewer_and_prompter` | Step 3 before Step 4/6 |
| `auto_mode_hard_deny_outranks_memory_deny` | Step 1 before Step 3 |
| `permission_memory_short_circuits_when_allowed` | Step 5 before Step 6 |
| `allow_always_persists_to_memory_as_session_grant` | Step 6 → writes Step 5 |
| `hook_allow_overrides_memory_deny` | Step 2 Allow overrides Step 3 |

The last scenario (`hook_allow_overrides_memory_deny`) is pinned by the
condition at `permission_gate.rs:109`:
```rust
if memory_effect == Some(PermissionEffect::Deny)
    && hook_permission.decision != Some(HookPermissionDecision::Allow)
```

A hook `Allow` in Step 2 prevents Step 3 from firing even when the memory
contains a Deny record.

---

## Order-invariant combinations (pinned assertion table)

```
hook=Allow  + memory=Deny  + reviewer=Deny  → Allow  (hook wins, Step 2)
hook=Deny   + memory=Allow + reviewer=Allow → Deny   (hook wins, Step 2)
hook=None   + memory=Deny  + reviewer=Allow → Deny   (memory, Step 3)
hook=None   + memory=Allow + reviewer=Deny  → Allow  (memory shortcircuit, Step 5)
hook=None   + memory=None  + reviewer=Deny  → Deny   (reviewer, Step 4)
hook=None   + memory=None  + reviewer=Allow → policy/prompter (Step 6)
```

These invariants are enforced by the unit tests listed above.
Any code change that breaks a test in `permission_gate.rs` must update this
document to reflect the new intended order.

---

*Last updated: Phase 5.3, 2026-05-14*
*Canonical source: `crates/runtime/src/conversation/permission_gate.rs`*
