# Sandbox Modes and Permission Mode Interaction

Phase 5.4 documentation.

**Important:** Anvil has two distinct subsystems named "sandbox" and "permission mode."
They are independent and operate at different layers. This document covers both and
documents where they interact.

---

## Two Separate Systems

### System A: SandboxMode / PermissionMode (write-boundary enforcement)

**File:** `crates/runtime/src/file_ops.rs` (SandboxMode enum, line 42;
`enforce_write_boundary`, line 186)

This is a process-scoped permission mode that gates every file write operation.
It is set at startup from CLI flags and updated live via `/permissions <mode>`.

Modes (stored as `AtomicU8` in `ACTIVE_MODE`):

| Mode | Write behavior |
|------|---------------|
| `Unset` | Treated as `WorkspaceWrite` (historical default) |
| `ReadOnly` | All writes denied unconditionally |
| `WorkspaceWrite` | Writes allowed inside project root + always-allowed paths (`/tmp`, `~/.anvil`, `$TMPDIR`) |
| `DangerFullAccess` | No path check; writes anywhere |

The `PermissionMode` enum in `crates/runtime/src/permissions/mod.rs` (line 8)
is the policy-layer analogue (used by `PermissionPolicy::authorize`). It has two
additional variants (`Prompt`, `Allow`) that represent interactive escalation
states and are not stored in the `ACTIVE_MODE` atomic.

### System B: SandboxConfig / namespace isolation (Linux-only process containment)

**File:** `crates/runtime/src/sandbox.rs` (`SandboxConfig`, line 29;
`build_linux_sandbox_command`, line 212)

This is a Linux-only namespace isolation system (`unshare`) that wraps tool
subprocesses in a separate PID/mount/network namespace. It is independent of
the write-boundary system.

Modes (`FilesystemIsolationMode`, line 10):

| Mode | Description |
|------|-------------|
| `Off` | No filesystem isolation |
| `WorkspaceOnly` (default) | Restricts filesystem access to workspace |
| `AllowList` | Restricts to explicitly listed mounts |

On non-Linux platforms (macOS, Windows, FreeBSD, etc.), `build_linux_sandbox_command`
returns `None` unconditionally (line 217: `if !cfg!(target_os = "linux")`), so
System B is inert outside Linux.

---

## Interaction Matrix

System A (write-boundary) and System B (namespace isolation) are **orthogonal**.
System A enforces at the Rust level before any subprocess is spawned; System B
constrains the subprocess's OS-level namespace. The two do not share code paths
and cannot override each other.

|  | Sandbox disabled (`SandboxConfig::enabled = false` or non-Linux) | Sandbox enabled, `WorkspaceOnly` | Sandbox enabled, `AllowList` | Sandbox enabled, `network_isolation = true` |
|--|--|--|--|--|
| **ReadOnly** | All writes denied at Rust level; no subprocess sandbox | All writes denied; namespace isolation also active but irrelevant to write denial | All writes denied; allow-list isolation also active | All writes denied; no outbound network from subprocesses |
| **WorkspaceWrite** | Writes to project root + always-allowed paths; no subprocess sandbox | Writes to project root; subprocess also namespace-isolated to workspace | Writes to project root; subprocess limited to allowed mounts | Writes to project root; subprocess has no network |
| **DangerFullAccess** | Writes anywhere; no subprocess sandbox | **Writes anywhere; namespace isolation still constrains subprocesses** | **Writes anywhere; allow-list still constrains subprocesses** | **Writes anywhere; subprocesses still have no network** |
| **Unset** | Same as WorkspaceWrite | Same as WorkspaceWrite + namespace | Same as WorkspaceWrite + allow-list | Same as WorkspaceWrite + no network |

**Key invariant (DangerFullAccess row):** `DangerFullAccess` bypasses System A
(write-boundary) but does **not** bypass System B (namespace isolation). This is
by design — `DangerFullAccess` opts the user out of the path-boundary check but
does not disable OS-level process containment, which is a separate opt-in
configured in `SandboxConfig`. This is confirmed at `file_ops.rs:85`:

```rust
fn bypass_sandbox() -> bool {
    if std::env::var("ANVIL_ALLOW_GLOBAL_WRITES").as_deref() == Ok("1") {
        return true;
    }
    matches!(active_sandbox_mode(), SandboxMode::DangerFullAccess)
}
```

`bypass_sandbox()` returns `true` for `DangerFullAccess`, which causes
`enforce_write_boundary` to return `Ok(())` without a path check. This function
is in `file_ops.rs` and has no knowledge of `SandboxConfig`. The `SandboxConfig`
enforcement path (in `build_linux_sandbox_command` / `resolve_sandbox_status`)
is entirely separate and is not affected by `bypass_sandbox()`.

---

## Subagent Inheritance

**Permission mode** (`SandboxMode` / `PermissionMode`) is inherited by subagents.
Phase 5.1 commit `7c406e5` wired `parent_permission_mode` into the subagent spawn
path. A parent running `DangerFullAccess` spawns subagents that also run
`DangerFullAccess`.

**SandboxConfig** is not explicitly threaded through the subagent spawn path in the
current codebase. Subagents launched via `TaskManager::create` inherit the
process-level sandbox state because they are subprocesses of the same Anvil
process, but `SandboxConfig` is not serialized into the delegation record and
is not explicitly re-applied to the subagent's tool subprocess chain. This means:

- A parent with `SandboxConfig::network_isolation = true` will propagate that
  constraint to its own tool calls.
- A delegated subagent (running as a new process invocation via the task runner)
  will use whatever `SandboxConfig` is set in its own session config, not the
  parent's runtime config.

This is a **known gap** rather than a bug — team delegation spawns a new
`anvil` process with its own config, not a shared-memory subagent. The
`parent_permission_mode` threading is a special-case injection; sandbox config
does not have an equivalent injection point. Filed as a Phase 5.5 follow-up item.

---

## Interaction with `ANVIL_ALLOW_GLOBAL_WRITES`

The environment variable `ANVIL_ALLOW_GLOBAL_WRITES=1` is a backward-compat
escape hatch that acts as an alias for `DangerFullAccess` in `bypass_sandbox()`
(`file_ops.rs:82-84`). It does not affect `SandboxConfig`. Like `DangerFullAccess`,
it bypasses System A only.

---

## ReadOnly is pre-sandbox

`enforce_write_boundary` checks `writes_forbidden()` (the ReadOnly test) as its
very first action, before any path lookup or sandbox check:

```rust
fn enforce_write_boundary(path: &Path) -> io::Result<()> {
    // ReadOnly: all writes denied, full stop.
    if writes_forbidden() { ... return Err(...) }
    // DangerFullAccess or env bypass: anywhere goes.
    if bypass_sandbox() { return Ok(()); }
    // WorkspaceWrite path check ...
}
```

This means ReadOnly denials are never visible to the Linux namespace sandbox —
the error is returned before any subprocess is started.

---

## AllowList without mounts

When `filesystem_mode = AllowList` and `allowed_mounts` is empty, `resolve_sandbox_status`
records a `fallback_reason` and sets `filesystem_active = true` but the allow-list
is effectively empty (`sandbox.rs:181-184`). This is a misconfiguration; the
behavior is "subprocess sees an empty mount list" which typically means all paths
are inaccessible. No automatic fallback to `WorkspaceOnly` occurs. Users must
supply at least one mount when using `AllowList` mode.

---

## Testing

Sandbox interaction tests live in `crates/runtime/src/sandbox.rs::tests` (line 286)
and `crates/runtime/src/file_ops.rs` (inline below `enforce_write_boundary`).

The matrix cells for DangerFullAccess + sandbox-enabled are not tested at the
integration level (no test spins up a Linux namespace and verifies that a
DangerFullAccess parent's tool subprocess is still namespace-constrained). This
is marked as a Phase 5.5 test coverage gap.

---

*Last updated: Phase 5.4, 2026-05-14*
*Canonical sources: `crates/runtime/src/file_ops.rs`, `crates/runtime/src/sandbox.rs`,
`crates/runtime/src/permissions/mod.rs`*
