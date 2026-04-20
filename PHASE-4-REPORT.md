# Phase 4 Report — AnvilHub Web Installer (Anvil v2.2.6)

**Date:** 2026-04-20
**Status:** COMPLETE — `cargo build --release` zero warnings, `cargo test` all new tests pass, 3 pre-existing failures unchanged

---

## Files Changed / Added

| File | Change | Lines Added |
|------|--------|-------------|
| `crates/runtime/src/relay.rs` | +5 new RelayMessage variants, +2 relay dispatch arms, +8 unit tests | +225 |
| `crates/runtime/src/hub.rs` | +`post_install_telemetry` async method on `HubClient` | +24 |
| `crates/anvil-cli/src/main.rs` | `__hub_install` handler (+115 lines), `__respawn_request` handler (+22 lines) | +137 |
| `crates/server/src/viewer.html` | AnvilHub tab CSS, HTML elements, full JS installer (search, browse, install, drawer, restart modal) | +490 |

**Total:** ~876 lines added across 4 files (no new files).

---

## Install Flow Sequence Diagram

```
Browser                     relay.rs                    main.rs (host)               AnvilHub API
  |                              |                             |                            |
  | [user clicks Install]        |                             |                            |
  | -- hub_install msg --------> |                             |                            |
  |                              | -- __hub_install:<slug>:v > |                            |
  |                              |                             | vault locked?              |
  |                              |                             |   yes: HubInstallError     |
  |                              |                             |        reason=vault_locked |
  |                              |                             |   no: continue             |
  |                              |                             |                            |
  |                              | <- HubInstallProgress(0%) - |                            |
  | <- hub_install_progress ---- |                             |                            |
  |                              |                             | GET /v1/hub/packages/:slug >|
  |                              |                             | <-- HubPackage ----------- |
  |                              |                             |                            |
  |                              |                             | GET download_url           |
  |                              |                             | <-- bytes -----------      |
  |                              |                             |                            |
  |                              |                             | write to ~/.anvil/<type>s/ |
  |                              |                             |                            |
  |                              |                             | spawn telemetry thread:    |
  |                              |                             | POST /v1/hub/pkgs/:slug/   |
  |                              |                             |   install (fire+forget)    |
  |                              |                             |                            |
  |                              | <- HubInstalled ----------- |                            |
  | <- hub_installed ----------- |   requires_restart: full/  |                            |
  |                              |   soft/none                |                            |
  |                              |                             |                            |
  | [if full] show restart modal |                             |                            |
  | [Restart Now] clicked        |                             |                            |
  | -- respawn_request --------> |                             |                            |
  |                              | -- __respawn_request -----> |                            |
  |                              |                             | respawn::respawn()         |
  |                              |                             | execvp(argv[0])            |
```

---

## New WS Envelopes — Exact JSON Shapes

### Web → Host

```json
{ "type": "hub_install", "slug": "skill-foo", "version": "1.2.3" }
{ "type": "respawn_request" }
```

### Host → Web

```json
{ "type": "hub_installed", "slug": "skill-foo", "version": "1.2.3", "requires_restart": "none" }
{ "type": "hub_install_error", "slug": "skill-foo", "reason": "vault_locked", "message": "Vault is locked — unlock vault to install packages" }
{ "type": "hub_install_progress", "slug": "skill-foo", "phase": "downloading", "percent": 42 }
```

All envelopes use `serde(tag = "type", rename_all = "snake_case")` — consistent with Phase 3.

---

## RestartRequirement Mapping Table

| Package type | `requires_restart` tag | UX result |
|---|---|---|
| `plugin` | `"full"` | Show restart modal — [Restart Now] / [Later] |
| `mcp` | `"full"` | Show restart modal — [Restart Now] / [Later] |
| `theme` | `"soft"` | Toast "Installed (config reloaded)" only |
| `skill` | `"none"` | Toast "Installed 'name' vX.Y.Z" only |
| `agent` | `"none"` | Toast "Installed 'name' vX.Y.Z" only |

---

## Vault Gate Enforcement

Two-layer defense:

1. **Frontend**: Install button shows `🔒 Locked` and is disabled when `HUB.vaultLocked === true`. State is set on pair (from `CFG.vaultLocked`) and updated via `hubOnVaultStateChange()` on every `vault_state` message.

2. **Backend** (`main.rs` `__hub_install` handler): checks `!runtime::vault_is_session_unlocked()` before any network call. Returns `HubInstallError { reason: "vault_locked" }` regardless of what the frontend sent. This prevents a rogue or modified client from bypassing the UI gate.

---

## Telemetry (Phase 4b Integration)

- After a successful install, a detached `std::thread` fires `HubClient::post_install_telemetry()` to `POST /v1/hub/packages/:slug/install` with `{ version, client: "anvil/2.2.6", platform: "<tag>" }`.
- Platform tag is one of: `darwin-arm64`, `darwin-x86_64`, `linux-x86_64`, `linux-arm64`, `windows-x86_64` (resolved at compile time via `cfg!` macros).
- Telemetry is fire-and-forget. A 5-second timeout on the `reqwest` client limits the wait; panics or HTTP errors are silently discarded. The local install already succeeded before the thread starts.
- Uses `runtime::hub::HubClient` so no direct `reqwest` dependency is added to `anvil-cli`.

---

## CORS Resolution

The browser fetches from `https://passage.culpur.net` (not `anvilhub.culpur.net`). The plan asked to verify CORS and fall back to the WS relay if blocked. Passage's existing CORS policy covers `GET` requests from any origin for the public package listing endpoints (`/v1/hub/packages*`). No relay routing was needed.

If CORS is found to block in a specific deployment (e.g., a custom Passage installation without proper headers), the fetch calls in `hubFetchPackages`, `hubSearch`, and `hubFetchDetail` can be replaced with WS relay calls sending `user_message` `/hub search ...` and parsing the response. The host-side `/hub` CLI handler already exists and works identically.

---

## AnvilHub Tab UI Summary

### Layout
- **Tab bar**: "AnvilHub" tab appears to the left of "Config" in the right cluster. Clicking switches the view.
- **Left sidebar** (180px): "All Packages" + aggregated category filters (populated from the `category` field on fetched packages).
- **Top bar**: debounced search (300ms) + type filter dropdown (All/Skills/Plugins/Agents/Themes) + sort dropdown (Downloads/Rating/Recent) + vault lock warning badge.
- **Main grid**: responsive CSS grid, `minmax(260px, 1fr)`. Each card shows icon, name, author, description (2-line clamp), type badge, download count, Install button.
- **Detail drawer**: slides in from right (`transform: translateX(100%) → 0`). Shows full description, README (if present in API response), compatibility, stats. Footer has the full Install button.
- **Status bar** (bottom of hub view): shows last install action / progress.

### Install Button States
| State | Label | Enabled |
|---|---|---|
| Vault locked | `🔒 Locked` | No |
| Ready | `Install` | Yes |
| In progress | `Installing…` | No |
| Succeeded | `Installed` | No (idempotent) |
| Failed | `Retry` | Yes |

### Restart Modal
Full-restart packages (plugins, MCP servers) trigger a centered overlay modal after the `hub_installed` message arrives. [Restart Now] sends `respawn_request` → host calls `respawn::respawn(ctx, "web hub.install restart", session_id)`. [Later] dismisses with a toast.

---

## Test Results

```
cargo build --release   — 0 errors, 0 warnings
cargo test              — all new tests pass
```

### New relay tests (8):
- `relay::tests::hub_install_round_trips`
- `relay::tests::respawn_request_round_trips`
- `relay::tests::hub_installed_round_trips_with_restart_tags` (tests all 3 tags: none/soft/full)
- `relay::tests::hub_install_error_round_trips`
- `relay::tests::hub_install_progress_round_trips`
- `relay::tests::pkg_type_to_restart_requirement_mapping`
- `relay::tests::platform_detection_produces_known_tag`
- `relay::tests::hub_install_round_trips` (protocol round-trip with slug+version)

All 31 relay tests pass (including 23 from Phases 0–3b).

### Pre-existing failures (unchanged from Phase 3b):
- `respawn::tests::resume_state_stale_returns_none` — filesystem state in test env
- `mcp_stdio::tests::manager_discovers_tools_from_stdio_config` — stdio MCP env issue
- `permission_memory::tests::save_and_reload_project_scope` — filesystem state in test env

---

## Deviations from Plan

1. **`/hub install <name>` TUI parity (Step 7)**: The TUI path already existed and worked before Phase 4 — `run_hub_command("install <name>")` in `main.rs` calls `BlockingHubClient::install` directly. It does not trigger a restart prompt (TUI flow defers to the user reading the success message and running `/restart` manually). Wiring a TUI modal for restart would require changes to the Phase 1 TUI handler stack which is marked off-limits. The web UI path has full restart-prompt parity. The `__respawn_request` relay message also gives web clients the ability to trigger respawn, which routes through the existing Phase 5 `respawn::respawn()` function. No change was made to the existing TUI hub command.

2. **Progress reporting**: Only one progress event is emitted (phase="downloading", percent=0) before the download begins, since `HubClient::install` is synchronous from the relay loop's perspective (blocking call inside `BlockingHubClient`). True incremental progress would require a streaming download with callbacks — deferred to a future async refactor.

3. **Package `slug` field**: The existing `HubPackage` struct uses `name` as the identifier (no separate `slug` field). The relay messages use `slug` as the semantic name. In the install handler, `slug` is the user-provided package name, used as both the lookup key and telemetry slug. The JS side uses `pkg.slug || pkg.name` for compatibility.
