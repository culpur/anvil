# Anvil ↔ Passage Relay Protocol — Current State (v2.2.18)

Task #706 webui-ssh audit, doc 2 of 3. Sister docs:
`ssh-tui-audit.md`, `ssh-webui-design-spec.md`.

Purpose: enumerate every wire message the host (TUI) ↔ passage ↔ browser
viewer exchange today, so the SSH design spec can layer new variants on
top without surprises.

---

## 1. Wire format + source-of-truth

- Encoding: JSON over WSS at `wss://api.culpur.net/v1/relay/sessions/{hash}?role={host|client}`.
- Single enum on the host side defines every variant:
  `crates/runtime/src/relay.rs::RelayMessage` (~530 LOC, lines 109–448).
- Serde tag: `#[serde(tag = "type", rename_all = "snake_case")]`.
- Drift gate: `KNOWN_RELAY_TAGS` (`relay.rs:551–619`) + the
  `relay_drift_gate_every_variant_is_known` test enforce that every
  Rust variant has a wire tag entry, with a `RelayDirection` annotation
  (HostToWeb | WebToHost | PassageInternal).
- Passage is **content-neutral by default** since v2.2.18 (task #647).
  The TS switch in `passage-culpur.net/src/routes/relay/sessions.ts`
  (~lines 136–249) only special-cases `pairing_*`, `session_meta`,
  `memory_snapshot`, `tab_opened/closed/renamed` for replay-cache
  semantics; **every other host event is forwarded verbatim** to every
  paired client. Web→host messages are filtered against an explicit
  switch (lines 309–392).
- Protocol version: `RELAY_PROTOCOL_VERSION: u32 = 1` (`relay.rs:1204`).

---

## 2. Full message catalogue (as of v2.2.18, commit 1b3e…)

### 2.1 Connection setup (passage-internal / pairing)

| Tag | Dir | Payload | Notes |
|---|---|---|---|
| `host_hello` | passage | `{ hash, protocol_version }` | First frame from host on ws.open. |
| `client_hello` | passage | `{ hash }` | First frame from client. |
| `client_connected` | passage→host | `{ client_id }` | Synthesised by passage when a new browser pairs. |
| `pairing_required` | host→client | (no fields) | Passage caches and sends if no paired peer yet. |
| `pairing_attempt` | client→host | `{ client_id, code }` | 6-digit code, 3 attempts. |
| `pairing_result` | host→client | `{ client_id, success, error? }` | On success, passage replays `session_meta` / `memory_snapshot` / `tab_opened` from its in-memory replay cache. |

### 2.2 Session data (host → web)

| Tag | Payload | Emitter |
|---|---|---|
| `session_snapshot` | `{ tabs: TabSnapshot[] }` | On pair (full state). |
| `text_delta` | `{ tab_id, text }` | Per streaming chunk from model. |
| `text_done` | `{ tab_id }` | End of streaming text. |
| `tool_start` | `{ tab_id, name, detail }` | Tool call kicks off. |
| `tool_result` | `{ tab_id, name, summary, is_error }` | Tool result returned. |
| `think_label` | `{ tab_id, label }` | "Thinking…" indicator. |
| `turn_done` | `{ tab_id }` | Turn complete. |
| `tokens` | `{ tab_id, input, output }` | After each turn. |
| `cost` | `{ tab_id, cost_usd }` | Per-tab USD (task #680.d). |
| `system` | `{ tab_id, message }` | Free-form system note pushed to tab scrollback. |
| `session_meta` | session_id, model, version, permission_mode, thinking_enabled, qmd_status?, block_time?, status_line_preset?, cost_type?, layout?, context_max?, build_sha? | Emitted on pair + on model/layout switch. |
| `memory_snapshot` | working, episodic, semantic, procedural, reflective, long_term, permission, qmd, qmd_latest, running_tabs, pending_perms, cost_usd | Per-turn debounce, drives the MEMORY rail. |

### 2.3 Tab lifecycle (host → web)

| Tag | Payload |
|---|---|
| `tab_opened` | `{ tab_id, name, model, session_id }` |
| `tab_closed` | `{ tab_id }` |
| `tab_renamed` | `{ tab_id, name }` |
| `tab_switched` | `{ tab_id }` |

### 2.4 Per-tab fan-out

Per-tab routing is **payload-level only** — every host emission carries
`tab_id` and the relay broadcasts to all paired clients; the viewer
dispatches on `tab_id` against its `tabs[]` map. There is no per-tab
WebSocket — one socket carries all tabs. The viewer's tab strip is the
only mechanism to render per-tab state.

### 2.5 Reverse channel (web → host)

The viewer sends JSON messages on the same WebSocket. The passage
client switch (sessions.ts:309–392) forwards a fixed allowlist:
`client_hello`, `pairing_attempt`, `user_message`, `request_new_tab`,
`request_close_tab`, `request_rename_tab`, `config_get`, `config_set`.

The host's WS read loop (`relay.rs:1017–1158`) dispatches the larger
allowlist, including the v2.2.18 additions:

| Tag | Payload | Host effect |
|---|---|---|
| `user_message` | `{ tab_id, message }` | Forwarded via `user_input_tx` mpsc to the TUI main loop; treated as a user-typed line in that tab. |
| `request_new_tab` | `{ name? }` | Bridged via `__new_tab:<name>` sentinel. |
| `request_close_tab` | `{ tab_id }` | `__close_tab:<id>` sentinel. |
| `request_rename_tab` | `{ tab_id, name }` | `__rename_tab:<id>:<name>` sentinel. |
| `request_focus_tab` (G2) | `{ tab_id }` | `__focus_tab:<id>`. |
| `request_layout` (G4) | `{ kind, tabs }` | `__layout_set:<kind>:<tabs>`. |
| `slash_dispatch` (G5) | `{ tab_id, command }` | `__slash_dispatch:<command>`. Whitelist enforced in `remote_control::slash_dispatch_route` (currently `schedule`, `daemon`, `remote-control`, `share` only — `/ssh` NOT in this list). |
| `config_get` / `config_set` (legacy) | { key, value } | `__config_get` / `__config_set:k:v` sentinels. |
| `config_update` (panel-aware) | `{ panel, field, value }` | `__config_update:<panel>:<field>:<json>` sentinel. |
| `hub_install` | `{ slug, version }` | `__hub_install:<slug>:<version>`. |
| `respawn_request` | (none) | `__respawn_request`. |
| `request_routine_approve` / `request_routine_reject` (G8) | `{ routine }` | `__routine_approve:<routine>`. |
| `permission_decision` (G10) | `{ tab_id, prompt_id, choice }` | `__permission_decision:<id>:<choice>`. |

Sentinel strings (the `__foo:bar` prefix protocol) are handled in the
TUI's `user_input_tx` consumer in `main.rs` (search for `__new_tab:` etc).

### 2.6 Host-emitted broadcasts unique to v2.2.18

- `slash_result` `{ tab_id, command, ok, output }` — output the
  dispatcher pushed into TUI scrollback after a `slash_dispatch`.
- `layout_changed` `{ kind, tabs }` — TUI-side layout switch.
- `daemon_status` — full anvild status snapshot.
- `proposal_snapshot`, `proposal_added`, `proposal_dropped` — routine
  proposal feed.
- `permission_prompt` `{ tab_id, prompt_id, prompt, options }` —
  per-tool approval prompt (G10).
- `config_snapshot`, `config_saved`, `config_error` — panel-aware
  config protocol.
- `vault_state` `{ locked: bool }` — emitted on pair + on any lock
  state change.
- `hub_installed`, `hub_install_error`, `hub_install_progress`.
- `peer_connected`, `peer_disconnected`, `error`.

Total wire tags as of v2.2.18: **53** (per `KNOWN_RELAY_TAGS`).

---

## 3. Passage replay cache semantics

`sessions.ts:178–225` caches the latest-wins of:

- `session_meta` (single instance — dedup by type).
- `memory_snapshot` (single instance — dedup by type).
- `tab_opened` (per-`tab_id` dedup so close+reopen evicts the stale one).
- `tab_closed` strips the matching `tab_opened` from the cache.
- `tab_renamed` mutates the cached `tab_opened.name`.

On every `pairing_result.success=true` the cache is replayed to the
newly-paired client. SSH-related events (terminal data, status) will
**not** want replay (they're per-session-time, lossy is acceptable);
the design spec calls this out.

---

## 4. Shape conventions

- `tab_id`: `usize` (Rust) → `number` (TS). Stable per TUI tab; 0 is
  the bootstrap tab.
- `client_id`: `string` (passage-assigned `c_<n>`).
- `prompt_id`: opaque string, must round-trip exactly.
- All-lowercase snake_case for both tag and field names. No camelCase
  on the wire.
- `serde(skip_serializing_if = "Option::is_none")` is the default for
  optional fields — JSON consumers must tolerate missing keys.

---

## 5. Authentication boundary

- Passage validates the **6-digit pairing PIN** on the relay
  (`PairingVerifier`, `relay.rs:39`) — host trusts passage's pairing
  decision and does NOT re-validate (`relay.rs:1052` comment).
- TLS terminates at Cloudflare → Apache → passage Node process.
- No browser-side persistent auth — the relay session hash + PIN is the
  full credential. Closing the tab discards everything except saved
  config in `localStorage` (e.g. `anvil_layout`).
- The relay process holds zero secrets — replay cache is wire-format
  payloads only, no decryption keys, no SSH credentials.

---

## 6. Per-tab event "fan-out"

There is **no fan-out at the relay layer**. Every paired client
receives every host event in source order. Per-tab routing is purely a
viewer-side concern (`viewer.html:822–928`):

- `tab_opened` adds an entry to `tabs[tab_id]`.
- `text_delta` calls `appendStreaming(msg.tab_id, msg.text)`.
- `tool_start` / `tool_result` etc all dispatch on `msg.tab_id`.
- The renderer only paints the **active** tab.

For SSH the design spec keeps this discipline: `ssh.*` events carry
`tab_id`, the viewer routes to per-tab xterm.js instances.

---

## 7. Known gaps the SSH spec must close

1. **No binary frame support.** vt100 byte streams ARE bytes, not
   UTF-8. The spec must either base64-encode them (~33% wire overhead)
   or upgrade to WebSocket binary frames. Default choice: base64 over
   the existing text-frame transport, no protocol upgrade. Revisit if
   sustained throughput becomes a bottleneck.
2. **`/ssh` not in slash-dispatch whitelist.** Webui parity routes
   `/ssh` through a dedicated `ssh.*` message family rather than
   `slash_dispatch`, so the legacy whitelist stays narrow and SSH gets
   independent rate-limit / auth gates.
3. **No flow control.** New `ssh.terminal_data` events will need
   sequence numbers (or a server-side throttle) to avoid swamping the
   viewer on `cat /dev/urandom`.
4. **No per-message rate limit at passage.** SSH connection attempts
   are credential-spray vectors — the design spec adds passage-side
   rate limit on `ssh.connect`.
