# Bug 3 Design: Per-Tab Parallel Inference

**Status:** DRAFT — awaiting user review before any code touches the tree
**Date:** 2026-05-11
**Companion doc:** `bug-3-audit.md` (read that first)

---

## Problem statement

Today, Anvil's TUI has multiple tabs but **only one of them can be running a
model turn at a time**. While tab A is streaming a response from Ollama, the
user cannot start a separate prompt in tab B. Slash commands typed during a
turn (like `/ssh`) are held until the turn finishes.

The user wants: **two tabs streaming two independent turns concurrently.**

The audit identified the single-turn bottlenecks:
- One `mpsc::sync_channel<TuiEvent>` for the whole TUI
- One `TuiSenderSlot` shared across all tabs
- One `ConversationRuntime` shared across all tabs
- `apply_tui_event` hardcodes `tab_id = self.active_tab` regardless of which
  worker thread emitted the event
- `wait_for_turn_end()` blocks the main loop until the active tab's turn
  finishes — no way to advance other tabs in the meantime
- Turn spawning uses `thread::scope` which means the main loop *can't* return
  while any turn is active

---

## Design decisions

For each of the 10 unknowns in the audit, here's the call:

### 1. Per-tab runtime vs. shared runtime

**Decision: per-tab `ConversationRuntime`.**

Each tab gets its own `ConversationRuntime` with its own session, model,
permission mode, allowed-tools set. Shared resources (vault, agent manager,
MCP servers, hooks config, OTel exporter) stay global behind `Arc<Mutex<>>`
where they already are.

Why: simpler mental model. "Tab = independent conversation." Reuses the
existing `build_runtime_with_tui_slot` factory unchanged. The only
duplicated state is the `DefaultRuntimeClient` (which holds a tokio runtime
+ provider client) — these are cheap to spin up (~ms) and the user is rate-
limited by the model anyway, so we'll never have hundreds of tabs.

The alternative (one runtime with per-tab session state) requires
refactoring every method on `ConversationRuntime` to take a `tab_id`
parameter and a session map. That's a much bigger change and creates a
session-swap dance every time we route between tabs.

### 2. Channel architecture

**Decision: tag every TuiEvent with `tab_id`, keep ONE channel.**

Change `TuiEvent` so every variant has a `tab_id: usize` field (or wrap it:
`struct TaggedEvent { tab_id, event }`). The single TUI receiver reads from
the channel, peeks at `tab_id`, and routes to the correct tab. The
`TuiSenderSlot` becomes `TuiSenderSlot<tab_id>`: each per-tab runtime gets
a *handle* that injects its tab_id into every send.

Why: minimal change to draw/render. The TUI still has one rx loop. Only the
"which tab does this go to" decision changes. Avoids the complexity of N
channels with N pollers, which would need a `select!` macro and could leak
channels on tab close.

This also leverages the audit finding that **the relay schema already has
`tab_id`** in every message — we're just teaching the in-process channel to
do what the relay schema has been documenting all along.

### 3. Esc / Ctrl+C with multiple in-flight turns

**Decision:**
- `Esc` cancels the **active tab's turn only**.
- `Ctrl+C` (first press, empty input) cancels the active tab's turn only.
- `Ctrl+C` (twice within 1s, empty input) exits Anvil — same as today,
  cancels all turns on the way out.
- `Ctrl+C` (with input) clears the input line only (no cancel) — same as today.

Why: matches user expectation that the "current" tab gets the action. If you
want to cancel a *background* tab, you switch to it first. Avoids the
ambiguity of "Esc cancels all of them."

### 4. Per-tab vs. global model

**Decision: per-tab model.**

Each tab already shows its own model name in the tab bar (tui/state.rs:239)
— this is currently display-only. Make it real. `Tab` gains a meaningful
`model: String` (it already has one but it's set to `cli.model.clone()` at
tab creation). Add a way to switch a tab's model via `/model` while in that
tab. Subagents (Team members) still inherit the active tab's model unless
overridden.

Why: lets the user run an Ollama session in tab A and a Claude session in
tab B simultaneously. This is the natural extension of per-tab session.

### 5. Permission prompts with concurrent tabs

**Decision: TUI modal prompt, per-tab queue.**

The current prompt uses blocking stdin (`render_permission_prompt`). With
parallel inference that breaks immediately. New approach:
- Each tab's runtime emits a `TuiEvent::PermissionRequired { tab_id, request }`.
- The TUI shows a modal **on the active tab only**, even if a background
  tab needs approval.
- The background tab's worker thread blocks on a `oneshot::Receiver<Decision>`
  until the user switches to that tab and approves/denies.
- A "pending approval" indicator appears in the tab bar (e.g. `[2: chat ⚠]`)
  so the user knows tab 2 is blocked on them.

Why: avoids the "two modals overlapping" UI nightmare. Honors the user's
focus: they decide which tab they're attending to.

### 6. Hook firing

**Decision: per-tab.**

Each tab fires its own SessionStart / SessionEnd / PreTurn / PostTurn hooks
against its own session. Hooks see the tab's `session_id` in their context.
Shared resources (the hooks config file) stay global.

Why: hooks already think in terms of session_id. Per-tab session = per-tab
hooks naturally.

### 7. Auto-compact

**Decision: per-tab.**

Auto-compact fires on a per-tab basis when that tab's session crosses the
threshold. Each tab compacts its own session independently. No cross-tab
coordination needed.

Why: trivially correct under per-tab runtime; matches the per-session
mental model.

### 8. Remote-control relay

**Decision: no schema change.**

The relay already includes `tab_id` in every message (audit Section 2,
tui/mod.rs:1547–1584). Once `apply_tui_event` routes correctly, the relay
forwarding will be correct too. The viewer.html already renders tabs.

Action item: manually verify viewer.html doesn't have a single-stream
assumption (audit Question 8). If it does, fix in the same change.

### 9. `wait_for_turn_end()` redesign

**Decision: rename + restructure.**

`wait_for_turn_end()` becomes `pump_events_until(active_tab_done)`. The
function continues to poll the same rx channel, but:
- Routes events to the correct tab via `tab_id`
- Returns when the **active tab's** turn is done, even if background tabs
  are still streaming
- Background tabs continue receiving events on subsequent main-loop iterations
  via a new `pump_events_nonblocking()` that runs as part of `read_input`'s
  idle work

Why: keeps the "submit input, get response" UX on the active tab unchanged.
Background tabs stream "in parallel" because the rx channel is being drained
on every read_input poll.

### 10. File drop

**Decision: file drop targets the active tab.**

Same as today, just respects per-tab session.

---

## Implementation plan

**Five commits, each independently buildable + testable:**

### Commit 1: TaggedEvent — make TuiEvent tab-aware

Files: `crates/runtime/src/tui_event.rs` (or wherever TuiEvent lives),
`tui/mod.rs`, `providers.rs`.

- Add `tab_id: usize` to every `TuiEvent` variant. (Or: introduce
  `TaggedTuiEvent { tab_id, event }` wrapper.)
- `TuiSender` becomes `TuiSender(Arc<TuiSenderInner>)` where the inner holds
  the channel plus the **default tab_id** for the runtime that owns it.
  When a runtime spins up for tab N, its sender stamps N onto every send.
- `apply_tui_event` reads `tab_id` from the event instead of `self.active_tab`.
- Compile + run existing tests. Behavior unchanged because we still only run
  one runtime, so all events carry the same `tab_id = active_tab`.

**Verification:** all 285 tests still pass; manual `anvil` run + send a turn,
verify no regression.

### Commit 2: Per-tab runtime in `Tab`

Files: `tui/state.rs`, `main.rs`.

- Add `runtime: Option<ConversationRuntime<...>>` to `Tab`.
- New tab creation calls `build_runtime_with_tui_slot` with the tab's
  `session_id` and a `TuiSender` stamped with the tab's `id`.
- Remove `LiveCli.runtime` from the "active tab data" path. (Keep one
  bootstrap runtime for non-tab operations like `--print`.)
- `cli.runtime` becomes `cli.tabs[active].runtime.as_mut().unwrap()` via a
  helper.

**Verification:** all tests pass; manual run: open `/tab new`, send a turn
in tab 2, verify model output goes to tab 2 and not tab 1.

### Commit 3: Concurrent turn spawn

Files: `main.rs` (the `thread::scope` block at 2738).

- Replace `thread::scope` with a per-turn `JoinHandle` map: `HashMap<usize,
  thread::JoinHandle<()>>` keyed by tab_id.
- The main loop no longer blocks on `wait_for_turn_end`; instead it calls
  `pump_events_nonblocking()` which drains the channel for ≤ N ms each
  iteration and routes to tabs.
- When the active tab's turn is in flight, the input box switches to
  "in-flight draft" mode (existing behavior).
- When a background tab's turn is in flight, the input box for the active
  tab is fully editable as usual.

**Verification:** open two tabs, send a long turn in tab 1, switch to tab 2,
send a different turn, verify both stream concurrently. Switch back to tab 1
mid-stream and verify the tab 1 stream is still alive.

### Commit 4: Permission prompt routing

Files: `providers.rs`, `tui/mod.rs`.

- `CliPermissionPrompter` no longer reads stdin directly. It emits a
  `TuiEvent::PermissionRequired { tab_id, request, response_tx }` (the
  `response_tx` is a `oneshot::Sender<Decision>`).
- TUI shows the prompt modal *only when the user is on that tab*; tab bar
  shows a `⚠` marker on tabs awaiting approval.
- The worker thread blocks on `response_rx` until the user decides.

**Verification:** open two tabs, fire tool calls that need approval in
both, verify the prompt shows for the active tab and the inactive tab gets
the marker. Approve both, verify both proceed.

### Commit 5: Cleanup + verification

- Audit Question 8 on viewer.html.
- Update `/help` Navigation section to describe parallel-tab semantics.
- Add an end-to-end test (in tests/) that spawns two tabs, runs a mock
  inference in each, verifies both complete with correct routing.
- Update `man/anvil.1` to mention per-tab parallel turns.

---

## Risks

1. **Channel bandwidth.** Today's bounded channel is 512. With two tabs both
   spewing TextDelta at peak streaming rates, we may queue up. Mitigation:
   measure; if needed, raise to 4096 or switch to per-tab subchannels.

2. **Tab switching during in-flight turn.** When user switches to tab 2
   while tab 1 is streaming, the rendering for tab 1's `pending_text` needs
   to stay correct so when they switch back, the streamed text is there.
   Today `flush_pending_text()` flushes on TurnDone. With per-tab pending
   buffers (which already exist in `Tab`), this Just Works.

3. **Session save during concurrent turn.** Two tabs could both write to
   their own session files concurrently. They're different files, so no
   conflict. But `persist_session` reads from "the runtime" — once that's
   per-tab, no shared mutation.

4. **Hook config reload.** If a hook fires while two turns are running and
   one of them modifies the hook config, the other turn might see the new
   config mid-flight. Existing race, not worse with parallel tabs. File as
   follow-up if it bites.

5. **OTel correlation.** Today's OTel events don't include tab_id. After
   this change, we'll add tab_id as an OTel attribute so traces can be
   filtered per-tab. Trivial — one line in `permission_decision()` and
   similar.

6. **Worst regression: silent breakage.** If Commit 1's TuiEvent tagging
   has a bug, every event routes to the wrong tab. Mitigation: Commit 1
   verifies behavior is unchanged because we're still only running one
   tab. The risk is real for Commits 2–4.

---

## Acceptance criteria

After all 5 commits land:

1. `anvil` → `/tab new` → in tab 1: "tell me a long story" → switch to tab
   2 → "what is 2+2?" → both streams complete without interfering.
2. `anvil` → `/tab new` → in tab 1 start a turn → in tab 2 type `/ssh` and
   press Enter while tab 1 is still streaming → SSH form opens immediately
   without canceling tab 1's turn.
3. All 285 existing tests pass.
4. New integration test for concurrent two-tab streaming passes.
5. Remote-control viewer.html shows two streams concurrently.
6. Manual smoke test: 10 turns alternating between two tabs, no panics, no
   wrong-tab rendering, no leaked threads (check with `ps`).

---

## Out of scope

- More than 2 concurrent tabs. The design supports N but I'll only test 2.
- Cross-tab references ("share this from tab 1 to tab 2"). Future work.
- Per-tab plugin sets. Today plugins are global; that doesn't change here.
- Persistent tab layouts ("restore my 3 tabs on next launch"). Future work.

---

## Estimated effort

- Commit 1 (TaggedEvent): 1–2 hours
- Commit 2 (per-tab runtime): 2–3 hours, real risk of breaking session save
- Commit 3 (concurrent spawn): 2–4 hours, the trickiest one
- Commit 4 (permission routing): 1–2 hours
- Commit 5 (cleanup + tests): 1–2 hours

**Total: 7–13 hours.** This is a full day or more of focused work, not a
session.
