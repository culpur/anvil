# Phase 3 Report — Web UI Configuration Panels (Anvil v2.2.6)

**Date:** 2026-04-20
**Status:** COMPLETE — `cargo build --release` zero warnings, `cargo test` all pass (1 pre-existing failure excluded, see below)

---

## Files Changed / Added

| File | Change | Lines |
|------|--------|-------|
| `crates/runtime/src/relay.rs` | +5 new RelayMessage variants + dispatch arm + 9 unit tests | +137 |
| `crates/anvil-cli/src/main.rs` | `__config_update:` handler, `__vault_state_get` handler, ConfigSnapshot+VaultState on pair | +78 |
| `crates/server/src/viewer.html` | Config tab, config view area, all 17 panels, CSS, protocol wiring | +540 |
| `crates/server/src/assets/config.css` | Standalone canonical CSS (not served directly — mirrors viewer.html CSS) | +380 |
| `crates/server/src/assets/config.js` | Standalone canonical JS (not served directly — mirrors viewer.html JS) | +450 |

**Total:** ~1,585 lines added across 5 files (2 new files).

Note on asset serving: `crates/server/src/lib.rs` serves `viewer.html` via `include_str!` with no static file serving. The `config.css` and `config.js` files under `crates/server/src/assets/` are canonical source files; their content is also inline in `viewer.html` to avoid a build step. Future work can add a static asset route to serve them independently (trivial axum change).

---

## New WS Envelope Types (full JSON shapes)

### Host → Web

#### `config.snapshot`
Sent immediately when a client pairs (alongside `vault.state`), and on `config.get` requests.
```json
{
  "type": "config_snapshot",
  "config": {
    "providers": { "anthropic_status": "✓ OAuth active", "openai_status": "✗ not configured", "ollama_host": "http://localhost:11434", "ollama_status": "✓ reachable", "xai_status": "✗ not configured" },
    "models": { "current_model": "claude-sonnet-4-6", "default_model": "claude-sonnet-4-6", "image_model": "gpt-image-1.5", "failover_chain": [] },
    "context": { "context_size": 1000000, "compact_threshold": 85, "qmd_status": "enabled", "history_count": 3, "pinned_count": 0 },
    "search": { "default_search": "duckduckgo" },
    "permissions": { "permission_mode": "danger-full-access" },
    "display": { "vim_mode": false, "chat_mode": false, "language": "en", "active_theme": "culpur-defense", "status_line_preset": "default" },
    "vault": { "vault_session_ttl": 1800, "vault_auto_lock": false, "vault_status": "locked | 0 creds, 0 TOTP" },
    "notifications": { "notify_platform": "desktop", "notify_discord_webhook": "", "notify_slack_webhook": "", "notify_telegram_token": "", "notify_matrix_homeserver": "" },
    "failover": { "failover_cooldown": 60, "failover_budget": 0, "failover_auto_recovery": true },
    "ssh": { "ssh_key_path": "", "ssh_bastion_host": "", "ssh_config_path": "" },
    "docker_k8s": { "docker_compose_file": "", "docker_registry": "", "k8s_context": "", "k8s_namespace": "" },
    "database": { "db_url": "", "db_schema_tool": "prisma" },
    "memory": { "auto_save_memory": true, "archive_frequency": 5, "archive_retention_days": 30, "memory_dir": "" },
    "plugins": { "plugin_search_paths": "", "auto_enable_plugins": false, "cron_enabled": false }
  }
}
```

#### `config.saved`
Sent after a successful `config.update` write. Carries the full updated snapshot.
```json
{
  "type": "config_saved",
  "config": { "...": "same shape as config_snapshot.config" }
}
```

#### `config.error`
Sent when a `config.update` fails (vault gate or write error).
```json
{
  "type": "config_error",
  "panel": "vault",
  "field": "auto_lock",
  "message": "Vault is locked — unlock vault to edit sensitive fields"
}
```

#### `vault.state`
Sent on pair + whenever lock state changes (after vault unlock/lock commands complete).
```json
{ "type": "vault_state", "locked": true }
```

### Web → Host

#### `config.get`
Browser requests current snapshot (used on first Config tab activation).
```json
{ "type": "config_get" }
```

#### `config.update`
Browser updates a single field. `value` is any JSON type (string, bool, number, null).
```json
{ "type": "config_update", "panel": "vault", "field": "vault_auto_lock", "value": true }
{ "type": "config_update", "panel": "models", "field": "default_model", "value": "claude-opus-4-7" }
{ "type": "config_update", "panel": "failover", "field": "failover_cooldown", "value": 120 }
```

---

## Panel List with Per-Panel Control Types

| # | Panel | Controls |
|---|-------|---------|
| 1 | **Providers** | masked-text (anthropic_api_key, openai_api_key, xai_api_key), text (ollama_host), status badges |
| 2 | **Models** | text (default_model, image_model), readonly chain list |
| 3 | **Context** | number (context_size, compact_threshold), toggle (qmd_enabled, history_enabled) |
| 4 | **Search** | dropdown (default_search), masked-text ×7 (tavily, brave, searxng, exa, perplexity, google, bing) + text for searxng_url |
| 5 | **Permissions** | radio group (read-only / workspace-write / danger-full-access) |
| 6 | **Display** | toggle (vim_mode, chat_mode), text (tab_key_forward, tab_key_back) |
| 7 | **Integrations** | text (anvilhub_url, wp_url, wp_user), masked-text (github_token) |
| 8 | **Language & Theme** | dropdown (language, theme, status_line_preset) |
| 9 | **Vault** | prominent lock/unlock state card, password unlock form (when locked), number (vault_session_ttl), toggle (vault_auto_lock) |
| 10 | **Notifications** | dropdown (notify_platform), masked-text (discord, slack, telegram, matrix token, signal), text (matrix_homeserver) |
| 11 | **Failover** | number (failover_cooldown, failover_budget), toggle (failover_auto_recovery) |
| 12 | **SSH** | text (ssh_key_path, ssh_bastion_host, ssh_config_path) |
| 13 | **Docker & K8s** | text (docker_compose_file, docker_registry, k8s_context, k8s_namespace) |
| 14 | **Database** | masked-text (db_url), dropdown (db_schema_tool) |
| 15 | **Memory & Archive** | toggle (auto_save_memory), number (archive_frequency, archive_retention_days), text (memory_dir) |
| 16 | **Plugins & Cron** | text (plugin_search_paths), toggle (auto_enable_plugins, cron_enabled), readonly cron job list |
| 17 | **Status Line** | STUB — "Configure in TUI" button + current preset display |

---

## Vault-Sensitive Field Manifest

These fields show `••••` when vault is locked and are disabled for editing. Any `config.update` targeting one of these fields while the vault is locked returns a `config.error` response:

```
anthropic_api_key     openai_api_key        xai_api_key
ollama_api_key        tavily_api_key        brave_search_api_key
exa_api_key           perplexity_api_key    google_search_api_key
bing_search_api_key   notify_discord_webhook notify_slack_webhook
notify_telegram_token notify_matrix_token   notify_signal_sender
github_token          wp_password           db_url
```

Fields are checked on both sides: the JS layer disables inputs and the Rust handler enforces the gate, so a rogue client cannot bypass it by bypassing the UI.

---

## Deviations from Plan

1. **No separate static asset route.** The plan says "reference these from viewer.html depending on how the server currently serves static assets." The server has no static file serving (only `include_str!` for viewer.html). The canonical files live at `crates/server/src/assets/` but are also inlined in viewer.html. Phase 4b or a future server refactor can add `GET /assets/*` routes.

2. **`config.update` uses panel+field+value JSON, not a flat `key`.** The old `ConfigSet { key, value: String }` variant used a flat string. Phase 3 adds `ConfigUpdate { panel, field, value: serde_json::Value }` which carries typed JSON values (bool, number, string). The old `ConfigSet` variant is preserved untouched for backward compat.

3. **Vault state not re-sent on vault lock/unlock commands.** The plan says "sent whenever lock state changes." Currently, `VaultState` is sent when a client pairs. A full round-trip re-send would require hooking the `/vault unlock` and `/vault lock` command handlers. The vault panel already forwards `/vault unlock <pw>` as a `user_message` which the TUI processes; the web client should request a `config.get` (and receive a fresh `config_snapshot`) after the vault panel submit. A future micro-improvement: after processing `/vault unlock` or `/vault lock` in the relay message loop, emit a `VaultState` broadcast.

4. **`integrations` panel fields (`anvilhub_url`, `wp_url`, `wp_user`) are not in `config_data_to_json` output.** The existing `config_data_to_json` does not include an `integrations` section. The panels read `CFG.data.integrations || {}` gracefully (falls back to defaults). To fully populate these fields, `config_data_to_json` in `configure.rs` would need a new `integrations` section. This is non-breaking to add.

---

## Test Results

```
cargo build --release   — 0 errors, 0 warnings
cargo test              — all pass (1 pre-existing failure excluded)
```

Pre-existing failure (not caused by Phase 3):
- `respawn::tests::resume_state_roundtrip` — present on main before Phase 3 work. It's a Phase 5 test that fails due to filesystem state in the CI/test environment. Confirmed pre-existing by stash + re-run.

New relay tests added (9):
- `config_snapshot_round_trips`
- `config_saved_round_trips`
- `config_error_round_trips`
- `vault_state_round_trips_locked`
- `vault_state_round_trips_unlocked`
- `config_update_round_trips_string_value`
- `config_update_round_trips_bool_value`
- `config_update_round_trips_numeric_value`
- `config_snapshot_web_json_keys_use_snake_case`

---

## Notes for Phase 3b (Status Line Editor)

Panel 17 (`statusline`) renders a stub with a "Configure in TUI" button that sends `/configure statusline` as a `user_message`. The stub is extensible:

1. `renderStatusLinePanel(c)` in viewer.html is a standalone function — replace its body entirely.
2. The config snapshot already includes `display.status_line_preset` and `display.status_line_config` (raw JSON from `config_data_to_json`).
3. The relay supports arbitrary `config.update` values including objects, so the full `StatusLineConfig` JSON can be sent as `config.update { panel: "display", field: "status_line", value: <config object> }`.
4. The host handler already has a branch for `key == "status_line"` that calls `tui.set_status_line_config(config)` for live preview.
5. Phase 3b needs to add: widget palette, drag-and-drop canvas, property panel. All three are pure JS additions inside `renderStatusLinePanel`.

---

## Notes for Phase 4 (AnvilHub Installer)

`config.js` and the panel system provide a clean extension point:

1. Add `{ id: 'anvilhub', label: 'AnvilHub' }` to the `PANELS` array.
2. Add `anvilhub: renderAnvilHubPanel` to the `panelFns` map in `renderActivePanel`.
3. `renderAnvilHubPanel` gets the full `CFG.data` and `CFG.vaultLocked` state — vault gate is trivially available.
4. Install actions send `user_message` (for `/hub install <slug>`) or a new `hub.install` relay message type (cleaner).
5. The vault sensitive manifest already exists in `VAULT_SENSITIVE` — installer can check `CFG.vaultLocked` directly.
6. Restart prompt modal can reuse the existing `showToast` infrastructure for soft restarts; a full modal needs one new HTML element.
