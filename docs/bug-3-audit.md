# Anvil Bug #3 Audit: Per-Tab Parallel Inference

**Date:** 2026-05-11  
**Status:** READ-ONLY AUDIT  
**Scope:** Complete surface map for per-tab parallel inference design  

---

## Section 1: Tab and Runtime State Today

### Tab Structure (`crates/anvil-cli/src/tui/state.rs:222`)

Each `Tab` is fully independent **per-tab** and contains:

```
pub(crate) struct Tab {
    pub id: usize,                                // Unique tab ID
    pub session_id: String,                       // UNIQUE session ID per tab
    pub log: Vec<LogEntry>,                       // Per-tab message log
    pub pending_text: String,                     // Streaming text accumulator
    pub scroll: usize, pub scrollback: ScrollbackBuffer,  // Per-tab scrollback
    pub input: String, pub cursor: usize,         // Per-tab input state
    pub model: String,                            // (Currently shared, see below)
    pub think_label: String, pub think_start: Option<Instant>,  // Per-tab thinking indicator
    pub input_tokens: u32, pub output_tokens: u32,  // Per-tab token counters
    // ... history, branches, completion popup, etc.
}
```

**KEY FINDING:** Every `Tab` already has:
- ✓ Unique `session_id` (file:line 240)
- ✓ Separate `log`, `pending_text`, scrollback state
- ✓ Independent token counters (input/output)
- ✓ Independent thinking indicator + start time
- ✗ **BUT:** model is set at tab creation time but all tabs share ONE runtime

### LiveCli Structure (`crates/anvil-cli/src/main.rs:5226+`)

The `LiveCli` struct holds:

```
pub struct LiveCli {
    pub session: SessionHandle,           // Currently 1 session
    pub runtime: ConversationRuntime<...>, // ⚠️ SINGLE RUNTIME — NOT PER-TAB
    pub model: String,                    // ⚠️ GLOBAL model
    pub tui_slot: TuiSenderSlot,         // ⚠️ SINGLE Mutex<Option<TuiSender>>
    pub agent_manager: Arc<Mutex<agents::AgentManager>>,  // Shared
    // ...
}
```

**CRITICAL FINDING:** The runtime, tui_slot, and model are **SHARED across all tabs**. When the user opens a new tab with `/tab new` (main.rs:2822), the new tab is just a new display state that shares `cli.runtime`.

### Turn Spawning Location (`main.rs:2738-2746`)

The turn is spawned in the main loop **within thread::scope**:

```rust
std::thread::scope(|s| {
    s.spawn(move || {
        if let Err(e) = cli_ref.run_turn(&input_owned) {
            // ...
        }
    });
    let _ = tui.wait_for_turn_end();
});
```

**File:line:** `crates/anvil-cli/src/main.rs:2738`

The closure captures `cli_ref` (mutable reference to `LiveCli`) and calls `run_turn(&input)`. The worker thread then calls `self.runtime.run_turn(...)` (line 5529, providers.rs not shown but called from main.rs:4657).

### Channel Flow

**Channel creation:** `crates/anvil-cli/src/tui/mod.rs:255`

```rust
let (tx, rx) = mpsc::sync_channel::<TuiEvent>(512);
```

- **Sender `tx`:** Cloned into `TuiSenderSlot` (Arc<Mutex<Option<TuiSender>>>) at line 256
- **Receiver `rx`:** Stored in `AnvilTui` struct at line 96
- **Size:** 512-element bounded channel (blocking send when full)

**Where TuiSender is cloned into runtime:**
- `crates/anvil-cli/src/providers.rs:553` — passed to `DefaultRuntimeClient::new()`
- `crates/anvil-cli/src/providers.rs:561` — passed to `CliToolExecutor::new()`

Both client and executor hold the sender and push `TuiEvent`s into the channel during streaming.

---

## Section 2: The TUI's Single-Turn Assumptions

All of these sites assume **only the active tab** can have a turn in flight:

| File:Line | Code Path | Assumption |
|-----------|-----------|-----------|
| `tui/mod.rs:1544` | `apply_tui_event()` | `let tab_id = self.active_tab;` — events always route to active tab |
| `tui/mod.rs:1589-1675` | Event dispatch | All `TextDelta`, `ToolCall`, `ThinkLabel`, `Tokens` modify `self.active_tab_mut()` |
| `tui/mod.rs:1820` | `wait_for_turn_end()` | Single `self.rx.recv_timeout(80ms)` loop — no routing by tab_id |
| `tui/mod.rs:1844-1890` | `handle_in_flight_key()` | Edits `self.active_tab_mut().input` while any turn is running |
| `tui/mod.rs:255-256` | `AnvilTui::new()` | One `mpsc::sync_channel` for the entire TUI |
| `tui/mod.rs:94-96` | `AnvilTui` struct | Single `active_tab: usize` + single `rx: Receiver<TuiEvent>` |
| `main.rs:2738-2746` | Main loop thread spawn | Single `tui.wait_for_turn_end()` call blocking on shared `rx` |
| `main.rs:255` | LiveCli | Single `pub tui_slot: TuiSenderSlot` shared into ONE runtime |

**Relay forwarding also encodes active_tab:**
- `tui/mod.rs:1547-1584` — `let tab_id = self.active_tab;` hardcoded when forwarding to relay (but relay message struct already has tab_id field!)

---

## Section 3: The Streaming/Runtime Layer

### Runtime Instantiation

**Where:** `crates/anvil-cli/src/providers.rs:462-569`

```rust
pub(crate) fn build_runtime_with_tui_slot(
    session: Session,
    model: String,
    system_prompt: Vec<String>,
    enable_tools: bool,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
    progress_reporter: Option<InternalPromptProgressReporter>,
    tui_slot: TuiSenderSlot,        // ← Shared slot
    agent_manager: Arc<Mutex<agents::AgentManager>>,
) -> Result<ConversationRuntime<DefaultRuntimeClient, CliToolExecutor>, ...>
```

**One runtime per LiveCli, not per tab.** When a new tab is created (main.rs:2822):

```rust
let new_session = create_managed_session_handle()?;
let tab_idx = tui.new_tab("new", cli.model.clone(), new_session.id.clone());
```

The new tab gets its own `session_id` but **uses the same `cli.runtime`**.

### The tui_slot Parameter

**What it is:** `Arc<Mutex<Option<TuiSender>>>`  
**File:line:** `main.rs:114`

```rust
pub(crate) type TuiSenderSlot = Arc<Mutex<Option<TuiSender>>>;
```

The `Option` allows the TUI to be toggled on/off (set to `None` for stdout mode). The runtime reads from this slot during streaming:

- `DefaultRuntimeClient` (providers.rs:660) holds the slot
- When streaming, it locks the slot: `if let Some(tx) = self.tui_slot.lock()...` and sends `TuiEvent`s

**ISSUE:** The same slot is used for all tabs. If Tab A and Tab B both try to send events, they both write to the **same channel**.

### Tokio Runtime & Threads

**Line 652 in providers.rs:**

```rust
pub struct DefaultRuntimeClient {
    runtime: tokio::runtime::Runtime,  // ← Tokio runtime
    client: ProviderClient,
    // ...
}
```

The `DefaultRuntimeClient` creates ONE tokio runtime. This runtime is used to make async API calls to providers (Anthropic, Ollama, etc.). The tokio runtime itself does NOT spawn parallel tasks across tabs — it's just the executor for the current turn's API request.

**No per-tab tokio runtimes exist today.** (File line 652, but also visible at line 497 where a tokio::runtime::Runtime is created during MCP tool discovery.)

---

## Section 4: Tool Execution + Permission Gates

### Permission Prompt Location

**File:line:** `providers.rs:612-619`

```rust
let response = render_permission_prompt(
    &request.tool_name,
    self.current_mode.as_str(),
    request.required_mode.as_str(),
    &input_summary,
    &mut stdout,
    &mut stdin,
);
```

The prompt is written to **stdout/stdin directly** — not through the TUI channel. This happens inside `CliPermissionPrompter::decide()` (line 582).

### Multiple Tabs at Permission Gate

**UNKNOWN:** Can two tabs both hit a permission prompt simultaneously? 

The prompt uses blocking stdin, so only ONE can be waiting at the prompt at a time. **But the question is whether the runtime infrastructure prevents two concurrent `run_turn` calls to begin with.** The runtime likely has its own mutual exclusion, but I cannot see it from the codebase alone.

---

## Section 5: Session Save + Auto-Compact

### persist_session()

**Location:** `main.rs` around line 2759 and throughout  
The session is persisted via `self.runtime.session()` which is a single session object shared across tabs.

**PROBLEM:** When two tabs write to disk concurrently, only ONE session file exists (one per tab's session_id). But the runtime holds **one session in memory**. So Tab A's changes and Tab B's changes both write to their own session files, but only if they each maintain separate runtime session objects.

**Currently:** All tabs share the same runtime → same in-memory session → persist writes ONE session file.

### Auto-Compact

**Location:** Compact slash command, line 4017 in main.rs  
Compact explicitly calls `self.runtime.compact()` on the shared runtime. Per-tab session isolation would need per-tab compact hooks.

---

## Section 6: Hooks, OTel, Remote-Control

### Hooks

**Location:** `main.rs:2133` (session_start), `3071` (session_end)

Hooks fire on the shared runtime's session start/end, not per-tab. They would fire once per session (global) not once per tab.

### OTel Events

**File:line:** `providers.rs:645` — `runtime::otel::permission_decision()` is called with tool_name and decision, no tab_id or session_id.

**Relay forwarding includes tab_id:**
- `tui/mod.rs:1547-1584` — `RelayMessage::TextDelta { tab_id, text }` and similar include `tab_id`
- The relay viewer.html on culpur.net/remote-control already has the infrastructure to handle multi-tab events

---

## Section 7: Prior Art — TeamDelegate

### TeamDelegate Overview

**Location:** `crates/runtime/src/team.rs` (not fully explored, but referenced in crates/tools/src/team_ops.rs)

**Task #297 commit message (from task list):** "TeamDelegate Ollama parallelism: route concurrent members through separate inference contexts"

**What TeamDelegate does:**
- Allows the user to create a `Team` with multiple `Member` subagents
- Each member gets its own `system_prompt`, `model` (optional), and `permission_mode`
- When a team task is delegated (`/team delegate`), multiple members run their own inferences in parallel
- **Each member likely gets its own runtime task** (based on the task description)

### How to Lift to Tabs

TeamDelegate shows that:
1. **Multiple independent system prompts** are feasible (each member has one)
2. **Multiple concurrent inferences** are feasible (separate inference contexts)
3. **Parallel event emission** is feasible (task #299 says TeamDelegate mirrors subagent tool-call events into parent scrollback)

The challenge: **TeamDelegate runs subagents from ONE parent turn.** Per-tab inference runs INDEPENDENT turns simultaneously. The channel architecture would need to:
- Route events by tab_id (not just hardcode `active_tab`)
- Spawn one worker thread PER TAB (not one per runtime)
- Each tab worker thread would need its own runtime (or tab-specific runtime session state)

---

## Section 8: Hardest Unknowns

The following questions CANNOT be answered from codebase inspection alone and MUST be resolved in the design phase:

1. **Should each tab have its own runtime, or should ONE runtime serve all tabs sequentially?**
   - **Implication:** Per-tab runtime = each tab holds its own MCP managers, LSP managers, and tool registries (duplication & memory cost)
   - **Alternative:** Shared runtime with per-tab session state = tab-aware session switching within one runtime (complex but efficient)

2. **How should the TUI channel be reorganized?**
   - Option A: One channel, but events include `tab_id` → TUI routes them (current structure)
   - Option B: Multiple channels (one per active turn) → each worker sends to its own channel
   - Option C: Hybrid — demultiplexed channel that routes by tab_id before appending to per-tab queues

3. **How should Esc / Ctrl+C work when two tabs both have turns in flight?**
   - Does Esc cancel the active tab's turn only, or all turns?
   - Does Ctrl+C (second press) exit the session, or cancel the active tab + stay in other tabs?

4. **Should two tabs share the same model, or each have independent model selections?**
   - Current: All tabs share one model (model is global in LiveCli)
   - Tab headers show different models today (tui/state.rs:239) but this is display-only
   - Design decision: Allow per-tab model override, or enforce global model across all tabs?

5. **How should permission approval prompts work if two tabs need different tool approvals simultaneously?**
   - Currently: Blocking stdin, so only one prompt at a time
   - Design: Show approval UI in-TUI for each tab independently? Or queue them?

6. **What happens to hook firing when two tabs are running concurrent turns?**
   - Should hooks fire per-tab or once per session?
   - If the turn_start hook modifies system_prompt, does it affect both tabs or just one?

7. **How should auto-compact schedule itself when multiple tabs are running?**
   - Does it compact the session shared by all tabs? (Currently yes)
   - If tabs have independent session state, do they compact independently? (Would require redesign of session API)

8. **Will the current RelayMessage structure (already tab-aware in viewer.html) work as-is, or does the schema need extension?**
   - Current: `TextDelta { tab_id, text }` — already has tab_id
   - Unknown: Does the web viewer correctly render parallel events from multiple tabs, or does it assume single-tab streaming?

9. **How should the TUI's `wait_for_turn_end()` be redesigned?**
   - Currently: Polls one `rx` and routes all events to `active_tab`
   - New: Could poll N channels (one per in-flight tab), or continue polling one multiplex channel and route by tab_id?

10. **Should stdin/file_drop (direct file input) be blocked while any turn is in flight, or only if a tab's turn is in flight?**
    - Current: `run_turn_file_drop()` (line 3466) uses the shared runtime
    - Design: Should file drops go to the active tab, or should they require no turns in flight?

---

## Summary: Minimal Change List for Design

To enable per-tab parallel inference, the **complete surface** is:

1. **TUI channel routing** (tui/mod.rs:96, :1542-1675)
   - Change: Add tab_id parameter to event dispatch, route by tab_id not active_tab

2. **wait_for_turn_end()** (tui/mod.rs:1744-1841)
   - Change: Poll N channels (or demux one channel by tab_id) for N in-flight turns

3. **Runtime per-tab split** (main.rs:LiveCli, providers.rs:build_runtime_with_tui_slot)
   - Change: Either create N runtimes (one per tab) OR refactor runtime to accept per-tab session state

4. **Turn spawning** (main.rs:2738-2746)
   - Change: Spawn one worker thread per in-flight tab, not block on one

5. **TuiSender slot** (main.rs:114, providers.rs:553-561)
   - Change: Either create N slots (one per tab) OR add tab_id routing at send site

6. **Permission prompt handling** (providers.rs:612-619)
   - Change: Route prompts to TUI modal (in-screen) instead of blocking stdin, or queue them per-tab

7. **Hook firing** (main.rs:2133, 3071)
   - Change: Decide hook scope (per-tab or global session) and refactor accordingly

8. **Remote-control relay** (tui/mod.rs:1547-1584)
   - Change: Verify viewer.html correctly renders concurrent tab events (likely already works)

---

**Estimated complexity:** HIGH. This is a fundamental architectural change to how the TUI and runtime interact. The per-tab session isolation is already in place (each tab has its own session_id), but the streaming pipeline (channel → TUI routing → draw) is hardcoded for single-turn-in-flight.
