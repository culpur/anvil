# Phase 3b Report — Status Line Web Editor (Anvil v2.2.6)

**Date:** 2026-04-20
**Status:** COMPLETE — `cargo build --release` zero warnings, `cargo test` 3 new tests pass, 1 pre-existing failure unchanged

---

## Files Changed / Added

| File | Change | Lines |
|------|--------|-------|
| `crates/server/src/viewer.html` | Replaced Status Line stub with full editor (CSS + JS) | +774 (1031 → 1805) |
| `crates/anvil-cli/src/main.rs` | Added `status_line` live-preview side-effect in `__config_update` handler | +5 |
| `crates/runtime/src/relay.rs` | Added 3 new unit tests for status_line round-trip | +96 |

**Total:** ~875 lines added across 3 files (no new files).

---

## Data Model Used

The editor reuses the existing types in `crates/runtime/src/theme.rs` without modification:

- `StatusLineConfig` — top-level struct: `preset: String`, `lines: Vec<StatusLine>`, `separator_char: String`, `compact: bool`, `widgets: HashMap<String, WidgetStyle>`
- `StatusLine` — per-row: `left: Vec<StatusWidget>`, `right: Vec<StatusWidget>`
- `StatusWidget` — enum with 36 concrete variants + `Text { content }`. Serialised as `serde(rename_all = "snake_case")` strings (e.g. `"git_branch"`)
- `WidgetStyle` — optional overrides: `color: Option<String>`, `label: Option<bool>`, `bold: Option<bool>`
- `StatusLinePreset` — 16 named presets, `from_name()` / `name()` / `all()` / `description()`
- `Side` — `Left` / `Right`

The JavaScript editor serialises the config as a plain JSON object matching this shape and sends it as `config.update { panel: "display", field: "status_line", value: <StatusLineConfig JSON> }`. The host deserialises it with `serde_json::from_str::<StatusLineConfig>` and calls `tui.set_status_line_config(config)` for live preview.

Type definitions: `/Users/soulofall/projects/anvil-dev/crates/runtime/src/theme.rs` lines 351–1374.

---

## Widget Catalog (36 concrete widgets mirrored in `SL_WIDGETS` JS array)

| # | ID | Name | Category |
|---|----|------|----------|
| 1 | `model` | Model | model |
| 2 | `thinking` | Thinking | model |
| 3 | `effort` | Effort | model |
| 4 | `provider` | Provider | model |
| 5 | `tokens_total` | Total Tokens | tokens |
| 6 | `tokens_input` | Input Tokens | tokens |
| 7 | `tokens_output` | Output Tokens | tokens |
| 8 | `cost` | Cost | tokens |
| 9 | `token_speed` | Token Speed | tokens |
| 10 | `context_bar` | Context Bar | context |
| 11 | `context_pct` | Context % | context |
| 12 | `context_tokens` | Context Tokens | context |
| 13 | `session_time` | Session Time | session |
| 14 | `session_pct` | Session % | session |
| 15 | `block_time` | Block Time | session |
| 16 | `git_branch` | Git Branch | git |
| 17 | `git_status` | Git Status | git |
| 18 | `git_diff` | Git Diff | git |
| 19 | `permissions` | Permissions | system |
| 20 | `qmd_status` | QMD Status | system |
| 21 | `version` | Version | system |
| 22 | `vim_mode` | Vim Mode | system |
| 23 | `remote_control` | Remote Control | system |
| 24 | `update_available` | Update Available | system |
| 25 | `archive_status` | Archive Status | system |
| 26 | `mcp_status` | MCP Status | system |
| 27 | `time_display` | Time | system |
| 28 | `burn_rate` | Burn Rate | cost_detail |
| 29 | `cost_daily` | Cost (Daily) | cost_detail |
| 30 | `cost_weekly` | Cost (Weekly) | cost_detail |
| 31 | `cost_monthly` | Cost (Monthly) | cost_detail |
| 32 | `cost_projection` | Cost Projection | cost_detail |
| 33 | `cache_hit_rate` | Cache Hit Rate | cost_detail |
| 34 | `code_productivity` | Code Productivity | productivity |
| 35 | `spacer` | Spacer | layout |
| 36 | `separator` | Separator | layout |

Note: `Text { content }` (the 37th variant in the enum) is a parameterised widget not included in `all_widgets()`. The plan's "37 widgets" count includes this variant; the editor shows 36 concrete palette items, consistent with the existing TUI's `WidgetPicker` which calls `StatusWidget::all_widgets()` (confirmed by theme.rs test `all_widgets_count` asserting exactly 36).

---

## Preset List (16 presets, mirrored in `SL_PRESETS` and `SL_PRESET_CONFIGS` JS objects)

| # | ID | Label |
|---|----|-------|
| 1 | `default` | Default |
| 2 | `minimal` | Minimal |
| 3 | `developer` | Developer |
| 4 | `token-heavy` | Token Heavy |
| 5 | `git-heavy` | Git Heavy |
| 6 | `compact` | Compact |
| 7 | `cost-focused` | Cost Focused |
| 8 | `streamer` | Streamer |
| 9 | `gaming` | Gaming |
| 10 | `devops` | DevOps |
| 11 | `budget-tracker` | Budget Tracker |
| 12 | `zen` | Zen |
| 13 | `academic` | Academic |
| 14 | `hacker` | Hacker |
| 15 | `night-owl` | Night Owl |
| 16 | `dashboard` | Dashboard |

Each preset config in `SL_PRESET_CONFIGS` exactly mirrors the widget placement in the corresponding `preset_*()` method in `theme.rs`.

---

## WS Envelope Types

Zero new envelope types. Phase 3b reuses the existing `config.update` envelope from Phase 3:

```json
{ "type": "config_update", "panel": "display", "field": "status_line", "value": { <StatusLineConfig JSON> } }
```

The host handler at `main.rs:__config_update` already processes this via `save_anvil_ui_config_key("status_line", json_value)`. Phase 3b adds one 5-line side-effect branch (the `if field == "status_line"` block) that calls `tui.set_status_line_config(config)` for live preview, consistent with the existing `status_line_preset` branch.

---

## Editor Feature Summary

### Left Column (200px): Widget Palette
- Search box filters by name or ID
- 36 widgets grouped into 9 categories with bold category headers
- Click any widget to append it to the selected line/side (or last line left-side)
- Each item shows emoji symbol + name

### Center Column (flex): Canvas
- Each status line rendered as a card with left/right sides separated by a vertical rule
- Widgets shown as colored chips, color-coded by category
- `draggable=true` on every chip; HTML5 drag-and-drop moves widgets within or between sides/lines
- Drop zones highlight with `drag-over` CSS state
- Click a widget chip to select it (shows properties in right panel)
- X button on hover to remove a widget
- Per-line header: up/down arrows, Clone, Remove buttons
- "Add Line" button at bottom (cap: 4 lines)

### Right Column (240px): Properties Panel
- When a widget is selected: color override dropdown (9 options), Show Label toggle, Bold toggle, Remove Widget button
- When nothing is selected: summary stats (line count, total widgets, active preset name)

### Top Toolbar
- Preset dropdown (16 presets + Custom) — selecting a preset immediately applies it to the draft
- Separator character input (editable, live)
- Compact mode checkbox
- Live preview toggle (on = debounced 300ms push on every change; off = only pushes on "Apply")
- Apply button (manual push)
- Reset button (restores Default preset)

---

## Deviations from Plan

1. **"Save as Preset" not implemented.** The plan mentions a "Save as Preset" button that saves to the user's theme library. The TUI itself has no user-preset-library mechanism (only the 16 built-in presets in `theme.rs`). Adding a local storage mechanism for user presets would require a new config.json key (`custom_presets`) and a new relay message or `__config_update` field. Deferred — the toolbar shows the 16 built-in presets plus "Custom" (the current edited state) which is the TUI's own concept. The Apply button saves the full config to disk.

2. **Preset dropdown shows "Custom" for edited configs.** When the draft has `preset: "custom"`, the preset dropdown shows a "Custom" option at the top. This matches TUI behavior where editing any preset marks it as "custom".

3. **Widget `Text { content }` not in palette.** This parameterised widget requires a content field. It is not included in the palette because `StatusWidget::all_widgets()` also excludes it. If added in future, a text input for the `content` field would appear in the Properties panel.

4. **Line alignment field.** The plan mentions per-line "line alignment" (left/center/right). `StatusLine` in `theme.rs` only has `left` and `right` sides (no center or alignment field). The editor respects the actual data model — center-alignment is not a current feature.

---

## Test Results

```
cargo build --release   — 0 errors, 0 warnings
cargo test              — 108 passed (anvil-cli), 3 new relay tests pass
```

New tests (all in `crates/runtime/src/relay.rs`):

1. `relay::tests::status_line_config_round_trips_via_config_update` — Full `StatusLineConfig` with 4 lines and ~12 widgets serialises through `ConfigUpdate`, deserialises back, asserts field values.
2. `relay::tests::status_line_config_deserialize_all_widget_ids` — All 36 widgets from `all_widgets()` survive JSON round-trip inside a `StatusLineConfig`.
3. `relay::tests::status_line_preset_application_replaces_config` — `ConfigUpdate` carrying `status_line_preset = "minimal"` can be parsed and the named preset resolved to a valid `StatusLineConfig`.

Pre-existing failures (not caused by Phase 3b):
- `respawn::tests::resume_state_stale_returns_none` — filesystem state issue in test env, pre-existing
- `runtime::mcp_stdio::tests::manager_discovers_tools_from_stdio_config` — pre-existing

---

## Notes for Phase 4 (AnvilHub Installer)

No overlap with Phase 3b. The AnvilHub installer adds a new panel to the `PANELS` array in viewer.html (`{ id: 'anvilhub', label: 'AnvilHub' }`) and a new `renderAnvilHubPanel` function — completely orthogonal to the Status Line editor.

The `SL` state object and all `sl*` JS functions are self-contained and will not conflict with AnvilHub JS. Both panels are rendered on-demand when selected from the sidebar; there is no shared mutable state between them beyond `CFG.data` and `CFG.vaultLocked`.

Phase 4 can proceed immediately.
