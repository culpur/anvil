# Hook System — Reference Documentation

Phase 5.4 documentation. Canonical source: `crates/runtime/src/hooks.rs`.

---

## Hook Events

All hook event types are declared in `crates/runtime/src/hooks.rs::HookEvent` (line 11).

| Event | Description | Payload fields | Decision injection? |
|-------|-------------|----------------|---------------------|
| `PreToolUse` | Fires before any tool call is executed | `tool_name`, `tool_input`, `tool_input_json` | Yes (exit 2 = deny) |
| `PostToolUse` | Fires after any tool call completes | `tool_name`, `tool_input`, `tool_output`, `tool_result_is_error` | Yes (exit 2 = deny; `continue_on_block` rewrites to warn) |
| `SessionStart` | Fires after config + MCP servers load, before first user prompt | `hook_event_name` | No (observe-only) |
| `SessionEnd` | Fires on clean exit (`Ctrl+C`/`Ctrl+D`, `/exit`, normal shutdown) | `hook_event_name` | No (observe-only) |
| `FileChanged` | Fires after Edit/Write/MultiEdit tool succeeds | `path`, `action` ("edit"\|"write"\|"create"\|"delete") | No (observe-only) |
| `CwdChanged` | Fires when the working directory changes mid-session | `old_cwd`, `new_cwd` | No (observe-only) |
| `PermissionRequest` | Fires when a permission prompt is about to be shown | `tool`, `input`, `requested_mode` | Yes (JSON `permissionDecision` field or exit 2 = deny) |
| `PermissionDenied` | Fires after any tool call is denied | `tool`, `input`, `reason`, `source` ("hook"\|"user"\|"sandbox") | No (observe-only) |
| `PostToolBatch` | Fires once per parallel tool batch after all tools complete | `tool_count`, `durations_ms`, `success_count`, `failure_count` | No (observe-only) |
| `Notification` | Fires when Anvil displays a notification to the user | `kind` ("permission_prompt"\|"error"\|"completion"\|"info"), `message` | No (observe-only) |

All payloads also contain `hook_event_name` (the event name as a string).  The
payload is passed as JSON on stdin to each hook process.

---

## HookSpec Variants

Three variants are available. All are declared in `crates/plugins/src/hooks.rs::HookSpec` (line 50).

### 1. Command (shell-wrapped)

The default form. Bare strings and tagged `{ "type": "command", "body": "..." }` objects
both resolve to this variant.

**Execution:** `sh -lc <command>` on Unix; `cmd /C <command>` on Windows.
Shell features (pipes, redirects, variable expansion) are available.

```json
// Bare string — backward-compatible
"PreToolUse": ["./hooks/pre.sh"]

// Explicit tagged form
{ "type": "command", "body": "./hooks/pre.sh" }
```

**Implementation:** `hooks.rs::HookRunner::run_command` (line 964),
dispatched via `shell_command()` (line 1224).

### 2. Exec (string[] form — no shell)

Array form. `args[0]` is the program; `args[1..]` are passed verbatim.

**Execution:** `Command::new(args[0]).args(&args[1..])` — no shell is involved.
Path placeholders (e.g. `{tool_name}`) in argument values are substituted without
quoting because no shell interprets them. This eliminates shell-injection risk when
interpolated values contain spaces or special characters.

```json
{ "args": ["./hooks/pre.sh", "{tool_name}"] }

// PostToolUse hook with continue_on_block
{ "args": ["./hooks/review.sh"], "continue_on_block": true }
```

**Implementation:** `hooks.rs::HookRunner::run_exec` (line 1031). The
`args[0].is_empty()` guard returns a Warn immediately rather than failing at spawn.

### 3. McpTool (MCP server dispatch)

Dispatches directly to a named MCP server method instead of forking a process.

**Execution:** Calls `McpHookInvoker::invoke(server, tool, input)`. Requires an
invoker to be registered via `HookRunner::with_mcp_invoker`. Without an invoker,
the entry is treated as a no-op warning — MCP hook failures never crash the turn.

```json
{
  "type": "mcp_tool",
  "server": "vault-scrubber",
  "tool": "redact",
  "input": { "field": "value" }
}
```

**Implementation:** `hooks.rs::HookRunner::run_mcp_tool_spec` (line 915).
`RuntimeHookSpec::from_json_value` (line 248) parses this variant before
falling back to `HookSpec` deserialization, so `mcp_tool` entries in
`settings.json` flow through config parsing without special handling.

---

## Execution Precedence

The dispatch order within `run_commands` (line 823) is:

1. **Prompt hook** — `plugin.is_prompt()` check first; injects a string into the
   next model turn without running any subprocess. Always allow outcome.
2. **Exec form** — `plugin.is_exec()` check second; spawns directly, no shell.
3. **Command (shell)** — default branch; wraps with `sh -lc` / `cmd /C`.
4. **McpTool** — dispatched via the registered invoker; no subprocess.

This order is per-spec within a single event's spec list. Specs run sequentially
in config order (config-sourced first, then `_extra` runtime-injected). The first
spec that emits `Deny` terminates the list immediately — remaining specs do not run.

---

## Permission Gate Precedence

For `PreToolUse` and `PermissionRequest` hooks, the hook chain runs **before** the
permission gate's policy check.

The full decision order is documented in `docs/research/PERMISSION-CHAIN.md`.
The relevant invariant: a `PermissionRequest` hook that returns `Allow` bypasses
`PermissionMemory::Deny`, the Reviewer agent, and `PermissionMemory::Allow`. This
is intentional — hooks represent plugin-level authority that can override even
user-recorded session memory.

**File:line:** `crates/runtime/src/conversation/permission_gate.rs:95` —
`hook_runner.run_permission_request(...)` fires before Step 3 (PermissionMemory Deny).

### PermissionRequest hook decisions

A `PermissionRequest` hook may inject a decision via its JSON stdout:

```json
{ "permissionDecision": "allow" }
{ "permissionDecision": "deny" }
{ "permissionDecision": "ask" }
{ "permissionDecision": "defer" }
```

Or by exiting with code 2 (deny). The first hook that emits a non-null decision
wins; subsequent hooks in the list still run but their decisions are ignored
(first-win semantics, implemented at `hooks.rs:647-675`).

---

## Exit Code Semantics

Applies to Command and Exec forms (McpTool maps `is_error: true` to Warn):

| Exit code | Outcome | Notes |
|-----------|---------|-------|
| 0 | Allow | stdout is captured as a message |
| 2 | Deny | stdout is the rejection reason; see `continue_on_block` |
| other | Warn | tool is allowed; warning message surfaced to user |
| signal | Warn | process killed by signal |

---

## `continue_on_block` semantics

Applies to `PostToolUse` hooks with the `Exec` variant only.

When `continue_on_block: true` and the hook exits with code 2, the `Deny`
outcome is rewritten to `Warn`. The model sees the rejection reason in the
message stream and continues the turn instead of hard-blocking.

**Implementation:** `hooks.rs::apply_continue_on_block` (line 1120).
The `apply_continue_on_block` function is a no-op for all events other than
`PostToolUse`, and for `continue_on_block: false` / unset.

---

## Fire Sites

Where each event is fired from:

| Event | Fire site | File:line |
|-------|-----------|-----------|
| `PreToolUse` | `run_allow_branch` (permission gate) | `conversation/permission_gate.rs:281` |
| `PostToolUse` | `run_allow_branch` after tool execution | `conversation/permission_gate.rs:319` |
| `SessionStart` | TUI startup after MCP load | `crates/anvil-cli/src/main.rs:2225` |
| `SessionEnd` | TUI shutdown | `crates/anvil-cli/src/main.rs:3640` |
| `FileChanged` | `run_allow_branch` after Edit/Write success | `conversation/permission_gate.rs:314` |
| `CwdChanged` | worktree switch tool | `crates/tools/src/worktree_ops.rs:24` |
| `PermissionRequest` | permission gate before policy check | `conversation/permission_gate.rs:95` |
| `PermissionDenied` | permission gate after deny from any source | `conversation/permission_gate.rs:62`, 115, 259, 286 |
| `PostToolBatch` | turn executor after parallel batch | `conversation/turn_executor.rs:147` |
| `Notification` | TUI notification dispatch | `crates/anvil-cli/src/main.rs:3537`, 4548, 4598 |

The `conversation/mod.rs` wrappers (`run_session_start_hooks`, `run_session_end_hooks`,
`run_cwd_changed_hooks`, `run_notification_hooks`) sit between the fire sites and
the `HookRunner` methods.

---

## Session Environment Variables

Both Command and Exec hooks receive these environment variables (in addition to
the inherited process environment):

| Variable | Source |
|----------|--------|
| `HOOK_EVENT` | Event name (e.g. `"PreToolUse"`) |
| `HOOK_TOOL_NAME` | Tool name for the call |
| `HOOK_TOOL_INPUT` | Tool input JSON string |
| `HOOK_TOOL_IS_ERROR` | `"1"` if the tool returned an error |
| `HOOK_TOOL_OUTPUT` | Tool output (PostToolUse only) |
| `ANVIL_SESSION_ID` | Session ID from `SessionContext` |
| `ANVIL_EFFORT` | Current effort level |
| `ANVIL_PROJECT_DIR` | Project directory path |
| `TRACEPARENT` | W3C trace context for distributed tracing (Command only) |

`ANVIL_SESSION_ID`, `ANVIL_EFFORT`, `ANVIL_PROJECT_DIR` are injected from the
thread-local `SessionContext` when available (Command: `hooks.rs:982-986`;
Exec: `hooks.rs:1059-1063`).

---

## Writing Hook Tests

### Test location

- `crates/runtime/src/hooks.rs::tests` — HookRunner unit tests (exit code
  semantics, MCP dispatch, `continue_on_block`, etc.)
- `crates/plugins/src/hooks.rs::tests` — HookSpec parsing and validation
  (serialization round-trip, `is_exec`, `continues_on_block` accessors)

### Pattern

```rust
#[test]
fn my_hook_test() {
    // Build a runner with a specific spec.
    let runner = HookRunner::new(RuntimeHookConfig::new(
        vec![HookSpec::Command("printf 'output'; exit 0".to_string())],
        Vec::new(), // post_tool_use specs
    ));

    let result = runner.run_pre_tool_use("Read", r#"{"path":"foo.rs"}"#);

    assert!(!result.is_denied());
    assert_eq!(result.messages(), &["output".to_string()]);
}
```

For MCP hook tests, implement `McpHookInvoker` on a mock struct and wire it via
`HookRunner::with_mcp_invoker`. See `hooks.rs:1394-1441` for the `MockMcpInvoker`
pattern used by the existing FEAT-30 tests.

For exec-form tests, use `HookSpec::Exec { args: vec![...], continue_on_block: None }`.
Exec tests that fork real processes are excluded from `#[cfg(target_os = ...)]`
guards in the test suite to avoid cross-platform failures.

---

*Last updated: Phase 5.4, 2026-05-14*
*Canonical sources: `crates/runtime/src/hooks.rs`, `crates/plugins/src/hooks.rs`*
*See also: `docs/research/PERMISSION-CHAIN.md`*
