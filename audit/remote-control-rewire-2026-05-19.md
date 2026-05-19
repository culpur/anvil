# Remote Control full rewire — audit
*Task #647 · branch `v2.2.18-install-arc` · 2026-05-19 · Maverick*

## Scope (locked by user)

Full TUI parity for the web viewer **and** daemon (`anvild`) awareness.
Bidirectional command path is the core deliverable — Web → Host must drive
slash commands, layout changes, focus shifts, and routine approvals, not
just observe.

## 8-axis contract reminder (`feedback-anvil-capability-contract`)

Every surface this rewire adds must trace through:

1. **def** — `RelayMessage` variant in `crates/runtime/src/relay.rs`
2. **registration** — host emitter wired (TUI / daemon poller / config)
3. **completion** — viewer handler that mutates browser state on receive
4. **handler** — host receive-side dispatch (`RelayHost::run` arm)
5. **dispatch** — TUI / runtime side-effect (`set_layout`, `run_schedule_command`, …)
6. **rendering** — viewer.html DOM mutation
7. **gate** — paired check, permission/tier gate, optional vault gate
8. **OTel+tests** — relay round-trip + viewer-handler unit test + drift gate

## Current state — 25 RelayMessage variants

Indexed against the 8 axes below.  ✅ = wired, ◯ = partial, ✗ = missing.

### Existing variants (host → web direction unless noted)

| # | Variant | def | reg | viewer | handler | dispatch | render | gate | tests | Notes |
|---|---------|-----|-----|--------|---------|----------|--------|------|-------|-------|
| 1 | `HostHello` | ✅ | ✅ | n/a | ✅ | ✅ | n/a | n/a | ✅ | passage-side |
| 2 | `ClientHello` | ✅ | n/a | ✅ | ✅ | ✅ | n/a | n/a | ✅ | passage-side |
| 3 | `ClientConnected` | ✅ | ✅ | n/a | ✅ | ✅ | n/a | n/a | ✅ | |
| 4 | `PairingRequired` | ✅ | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ | |
| 5 | `PairingAttempt` | ✅ | n/a | ✅ | ✅ | ✅ | n/a | n/a | ✅ | web→host |
| 6 | `PairingResult` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | n/a | ✅ | |
| 7 | `SessionSnapshot` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | sent on pair |
| 8 | `TextDelta` | ✅ | ✅ | ✅ | n/a | ✅ | ✅ | ✅ | ✅ | #289 fix shipped |
| 9 | `TextDone` | ✅ | ✅ | ✅ | n/a | ✅ | ✅ | ✅ | — | |
| 10 | `ToolStart` | ✅ | ✅ | ✅ | n/a | ✅ | ✅ | ✅ | — | |
| 11 | `ToolResult` | ✅ | ✅ | ✅ | n/a | ✅ | ✅ | ✅ | — | |
| 12 | `ThinkLabel` | ✅ | ✅ | ✅ | n/a | ✅ | ✅ | ✅ | — | |
| 13 | `TurnDone` | ✅ | ✅ | ✅ | n/a | ✅ | ✅ | ✅ | — | |
| 14 | `Tokens` | ✅ | ✅ | ✅ | n/a | ✅ | ✅ | ✅ | — | |
| 15 | `System` | ✅ | ✅ | ✅ | n/a | ✅ | ✅ | ✅ | — | |
| 16 | `TabOpened` | ✅ | ✅ | ✅ | n/a | ✅ | ✅ | ✅ | — | |
| 17 | `TabClosed` | ✅ | ✅ | ✅ | n/a | ✅ | ✅ | ✅ | — | |
| 18 | `TabRenamed` | ✅ | ✅ | ✅ | n/a | ✅ | ✅ | ✅ | — | |
| 19 | `TabSwitched` | ✅ | **✗** | ◯ | n/a | **✗** | ◯ | ✅ | — | **GAP 1** — no host emitter when TUI flips active tab |
| 20 | `SessionMeta` | ✅ | ✅ | ✅ | n/a | ✅ | ✅ | ✅ | — | |
| 21 | `RequestNewTab` | ✅ | ✅ | ✅ | ✅ | ✅ | n/a | ✅ | — | web→host |
| 22 | `RequestCloseTab` | ✅ | ✅ | ✅ | ✅ | ✅ | n/a | ✅ | — | web→host |
| 23 | `RequestRenameTab` | ✅ | ✅ | ✅ | ✅ | ✅ | n/a | ✅ | — | web→host |
| 24–25 | `Config*`, `Vault*`, `Hub*` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | full panel protocol shipped |
| 26 | `UserMessage` | ✅ | n/a | ✅ | ✅ | ✅ | n/a | ✅ | — | web→host typed prompt |
| 27 | `PeerConnected` / `PeerDisconnected` / `Error` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | |

### Gaps that block "full TUI parity"

| ID | Gap | Where felt in viewer | Fix |
|----|-----|----------------------|-----|
| G1 | No `TabSwitched` emit when host TUI swaps active tab via `Tab` / `Ctrl+T` / mouse | Web viewer's active-tab indicator is stale | Emit `TabSwitched { tab_id }` from TUI on focus change |
| G2 | Web cannot request a tab focus change | Viewer can click a tab but host stays on the original | New `RequestFocusTab { tab_id }`, dispatched into TUI |
| G3 | No layout-change event | Web viewer pane structure never reflects `vertical_split` / `three_pane` / `journal` | New `LayoutChanged { kind, tabs }`; web mirrors layout |
| G4 | Web cannot change layout | No layout picker | New `RequestLayout { kind, tabs }` → `set_layout` |
| G5 | No slash-command dispatch path | Web can only send raw prompts; cannot `/schedule pending` etc. | New `SlashDispatch { tab_id, command }` + `SlashResult { tab_id, output, ok }` |
| G6 | No daemon status feed | Viewer cannot show `pending_proposals_total`, last tick, anvild PID | New `DaemonStatus { … }` snapshot, polled from `~/.anvil/run/anvild.status.json` every 5 s |
| G7 | No routine-proposal feed | Viewer cannot list pending proposals | `ProposalSnapshot { proposals: [...] }` on pair + `ProposalAdded`/`ProposalDropped` deltas |
| G8 | Web cannot approve / reject proposals | No approve/reject buttons | New `RequestRoutineApprove { routine }` / `RequestRoutineReject { routine }` — routes through `/schedule approve` / `/schedule reject` |
| G9 | MEMORY block + status footer not mirrored | Viewer has no badge for the new `RoutineProposals` status widget | Carried implicitly by G6 daemon snapshot — viewer renders badge from `pending_proposals_total` |
| G10 | Permission prompts (ask-tier inline) are TUI-only | Web user cannot allow/deny a tool call | New `PermissionPrompt { tab_id, prompt_id, prompt, options }` + `PermissionDecision { tab_id, prompt_id, choice }`.  v2.2.18 scope: **emit + ack pair** (no per-tool wiring yet — see "Deferrals" below) |
| G11 | Drift gate missing | Nobody enforces that every variant has a host emitter or a viewer handler | Add a runtime test that walks `RelayMessage::all_variants()` and asserts a `direction()` + `axis_map` for each |

### Idle-redraw + flooding (BUG-122-8)

Today every `TuiEvent` round-trips a separate WebSocket frame.  The new
daemon-status feed must NOT make this worse.  Approach:

- Daemon-status poll runs once every 5 s and only emits when the JSON
  bytes differ from the last emit (cheap memcmp).
- Layout-change emit is event-driven (already cheap).
- Proposal feed is delta-coded; the full snapshot is only sent on pair.

## New variants this rewire adds

(All snake_case via `#[serde(rename_all = "snake_case")]`.)

```rust
// G1 — host → web
TabSwitched { tab_id: usize }                  // already exists; new emitter only
// G2 — web → host
RequestFocusTab { tab_id: usize }
// G3 — host → web
LayoutChanged { kind: String, tabs: bool }     // kind: "classic" | "vertical_split" | "three_pane" | "journal"
// G4 — web → host
RequestLayout { kind: String, tabs: bool }
// G5 — bidirectional pair
SlashDispatch { tab_id: usize, command: String }   // web → host
SlashResult   { tab_id: usize, command: String, ok: bool, output: String }  // host → web
// G6 — host → web
DaemonStatus {
    running: bool,
    pid: Option<u32>,
    last_tick_at: Option<u64>,
    routines_loaded: usize,
    routines_fired_last_tick: usize,
    pending_proposals_total: usize,
    last_error: Option<String>,
    anvil_version: Option<String>,
}
// G7 — host → web
ProposalSnapshot { proposals: Vec<ProposalSummary> }
ProposalAdded   { proposal: ProposalSummary }
ProposalDropped { routine: String }
// G8 — web → host
RequestRoutineApprove { routine: String }
RequestRoutineReject  { routine: String }
// G10 — bidirectional pair
PermissionPrompt   { tab_id: usize, prompt_id: String, prompt: String, options: Vec<String> }  // host → web
PermissionDecision { tab_id: usize, prompt_id: String, choice: String }                        // web → host
```

`ProposalSummary` mirrors fields the viewer needs to render an
approve/reject row:

```rust
struct ProposalSummary {
    routine: String,
    schedule_raw: String,
    permission_mode: String,   // "accept" | "plan" | "auto" | "danger"
    prompt_preview: String,
    scheduled_at: u64,
    proposed_at: u64,
}
```

## Host-side plumbing

1. **TUI emits** — extend `apply_tagged_event` and the existing `set_layout` / tab-focus paths in `crates/anvil-cli/src/tui/mod.rs` to `relay_forward(TabSwitched | LayoutChanged)`.
2. **Daemon poller** — new task spawned from `remote_control.rs` start path.  Reads `~/.anvil/run/anvild.status.json` and `proposal::list_pending(home, now)` every 5 s; emits `DaemonStatus` + `ProposalSnapshot` deltas when changed.  De-dupe by hashing serialized JSON bytes.
3. **Slash dispatch** — `RelayMessage::SlashDispatch` arrives at `RelayHost::run`; forwarded via `user_input_tx` with prefix `__slash_dispatch:<tab_id>:<command>`.  `main.rs` event loop catches the prefix, routes through the canonical `commands::dispatch::dispatch_slash_command`, captures the returned String, emits `SlashResult` back over the relay.
4. **Approve / reject** — `RequestRoutineApprove { routine }` → `__routine_approve:<name>` sync message → `schedule_cmds::run_schedule_command(Some(&format!("approve {name}")))`; emit the rendered string back as `SlashResult` and emit `ProposalDropped { routine }`.

## Web-side plumbing

`viewer.html` rewrite is structural but bounded:

1. **Per-tab pane router** — replace the single scrollback with a tab → pane
   map.  Each tab has its own scrollback DOM node; the active tab is the
   only one mounted in the visible region.
2. **Layout mirror** — when `LayoutChanged` arrives, swap the active pane
   container's CSS class (`layout-classic` / `layout-vertical-split` /
   `layout-three-pane` / `layout-journal`).  Static CSS rules style each.
3. **Sidebar — Pending routine approvals** — new fixed-right column
   driven by `ProposalSnapshot` + `ProposalAdded` + `ProposalDropped`.
   Approve / Reject buttons emit `request_routine_approve` /
   `request_routine_reject` on click.
4. **Daemon badge** — bottom-bar widget driven by `DaemonStatus`.
   States: `dead` (no PID), `idle` (running, 0 fired last tick),
   `firing` (>0), `error` (`last_error` non-null).
5. **Slash bar** — new `/`-prefixed input that emits `SlashDispatch` and
   appends the resulting `SlashResult.output` to the current tab's pane.
6. **Layout picker** — dropdown that emits `RequestLayout`.
7. **Idle redraw guard** — viewer no longer redraws on every frame.
   Each event handler sets a `dirty` flag; a single `requestAnimationFrame`
   loop flushes mutations.

## Deferrals (explicit, not silent)

Per `feedback-no-silent-deferral`, these are *named* gaps with a follow-up
task filed below. **None are required to claim Task #647 done** as long as
the bidirectional plumbing they sit on top of is wired:

- **D1 (G10 follow-up): per-tool permission prompt sites.**
  The `PermissionPrompt` / `PermissionDecision` *protocol* and *viewer
  handler* ship in #647.  Wiring every individual tool's
  ask-tier prompt site (bash, write, fetch, …) to actually
  use this round-trip is large enough to be its own task.  Sites
  identified: `runtime/src/permissions/`, `runtime/src/hooks.rs`.
  → **Follow-up task #648** *Permission prompt: relay every per-tool prompt over Remote Control.*

- **D2 (G3 follow-up): viewer pixel-perfect parity with three-pane / journal layouts.**
  v2.2.18 #647 ships the `LayoutChanged` event + a CSS class swap.  Replicating
  every detail of the ratatui `three_pane.rs` and `journal.rs` renderers
  (e.g. timestamp gutter, FOCUS pane highlight) is layout-design work
  that does not affect the bidirectional contract.
  → **Follow-up task #649** *Viewer: pixel-parity for three-pane + journal layouts.*

Both follow-up IDs are placeholders to be filed in the user's tracker
once this rewire lands.  They are NOT being silently skipped — the
protocol surface and tests required for #647 ship in this branch.

## Test plan

Baseline (measured 2026-05-19):

- `cargo test -p runtime --lib relay::` → **31 passed**
- `cargo test -p runtime --lib remote::` → 6 (TBD verify)
- `cargo test -p server` → 2

New tests this rewire must add:

- **R1** Each new variant has a JSON round-trip test.  (One test per new variant — 13 new tests.)
- **R2** Drift gate (`relay_drift_gate`) — for every `RelayMessage` variant the discriminant string must appear in a static `KNOWN_VARIANTS` constant *and* be marked in `axis_map` as having a host emitter or a viewer handler.
- **R3** Host-side mock test: feed a `SlashDispatch { command: "schedule list" }` into the dispatcher and assert a `SlashResult` is returned with non-empty output.
- **R4** Daemon-poller change-detection: write two different `anvild.status.json` bodies; assert poller emits two `DaemonStatus` frames, but emits zero when the body is byte-identical to the last.
- **R5** Proposal delta: pre-populate two pending proposals, snapshot, write a third → `ProposalAdded`; drop one → `ProposalDropped`.
- **R6** Idle-redraw regression: feed 100 identical `DaemonStatus` polls in a row; assert exactly one frame is sent.

## Verification

Before claiming done:

```
cargo build --workspace
cargo test -p runtime -p commands -p anvil-cli
```

Reported test counts will be: previous-total → new-total per crate, with
diff per file.

## Anti-regressions to confirm

- `feedback-tui-stdout-anti-pattern`: no new `println!`/`print!` in relay /
  remote_control / daemon-poller code paths.  All warnings via `tui::log_warning`.
- `feedback-no-influence-attribution`: no external project names leak into viewer copy.
- Text in viewer.html user-facing strings uses **CC**, not "Claude" or
  "Claude Code" (`feedback-cc-only-naming`).  Tool/system names in code
  (e.g. "ratatui", "WebSocket") are fine.
- Public-facing copy must not leak `registry.culpur`, internal IPs, or
  Aegis hostnames.  `passage.culpur.net` is the only public host the
  viewer references (it's the relay endpoint, already public).
- BUG-122-8: idle-redraw stays under 1 frame/s in steady state.

## Roll-out order (small commits)

1. **Slice 1 — audit + RelayMessage variants** (this commit).  Adds the
   new variants + their round-trip tests.  Zero TUI/viewer behaviour change.
2. **Slice 2 — drift gate (R2).**  Pure test, no behaviour.
3. **Slice 3 — host emitters** for `TabSwitched`, `LayoutChanged`.
4. **Slice 4 — daemon poller + proposal feed.**
5. **Slice 5 — slash-dispatch round-trip** (`SlashDispatch` / `SlashResult`).
6. **Slice 6 — routine approve / reject** plumbing.
7. **Slice 7 — viewer.html rewrite** (per-tab panes, layout mirror,
   approvals sidebar, slash bar, layout picker).
8. **Slice 8 — verification + final commit** (`cargo build --workspace` +
   relay/commands/anvil-cli test suites green).

Each slice is a separate commit and push.
