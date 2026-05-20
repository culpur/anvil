# Anvil v2.2.18 — Web Viewer Parity, Mouse Capture Default-OFF, Autocompact Hardening

Released: 2026-05-20

v2.2.18 is a focused correctness and polish release. Three issues that have quietly burned users for weeks get resolved in the same cycle: the autocompact threshold was measuring against the output cap instead of the context window (meaning sessions on long-context models compacted 10–50× too early), mouse capture defaulted ON when it should have been OFF, and the web viewer was missing a coherent tab routing layer. The passage-side relay rewrite lands alongside the TUI fixes so the viewer finally has the same tab-aware session model as the desktop app. Seventeen commits over v2.2.17.

## Web Viewer Parity

The `passage` relay and AnvilHub viewer received a complete tab-routing rewrite to match the TUI's per-tab architecture.

### Tab system end-to-end

`/tab new`, `/tab rename <name>`, and `/tab switch <n>` are now first-class slash commands. `Ctrl+T` broadcasts a new-tab request to the relay. Tab IDs are stable (generated once at creation, never reused) and the relay routes `user_message` and `slash_result` events to the correct tab's stream rather than broadcasting to every connected client. The bug that made `/tab` commands appear to succeed in the TUI but have no effect in the viewer is gone.

Per-tab routing required removing the `paired_count` gate that was incorrectly blocking routing after the first paired connection. The gate was meant to prevent duplicate events on multi-viewer setups but instead blocked legitimate single-viewer use after any reconnect.

### Viewer default layout

The viewer's default layout is now `vertical_split + tabs`, matching the TUI default introduced in v2.2.16. Fresh sessions open with the rail on the left and the main deck on the right.

### Relay parity

Cost labels in the viewer's status footer now show the correct `cost_type` chip (OAuth / local / cloud) rather than a fabricated dollar figure. `MemorySnapshot` events are cached and broadcast to the viewer so the memory rail populates correctly on reconnect. `SessionMeta` carries `context_max` and `build_sha` so the viewer can render an accurate context-window progress bar.

Default-allow forwarding: messages from the viewer that don't match a specific handler now forward to the TUI's active tab instead of being silently dropped. Slash completion responses are forwarded back to the originating viewer connection.

Collapsible tool cards in the viewer's scrollback reduce visual noise on long agentic runs. The cost-type chip appears in the status footer on every message.

### slash bar always visible

The viewer's slash-command bar is now always visible in the header (not hidden until you type `/`). `Cmd+K` opens the command palette from any state.

## Autocompact Hardening (#697 — critical)

The most user-visible fix in v2.2.18: `maybe_auto_compact` was computing the compaction threshold against `max_output_tokens` (typically 8K–16K) rather than `context_window` (64K–200K+ depending on model). On claude-sonnet-4-5 with a 200K context window, this meant autocompact fired when the session reached roughly 80% of 8K output tokens — around 6K input tokens. Sessions that should comfortably hold 100K tokens of context were being compacted after a handful of turns.

The fix: threshold computation now reads `session.context_window` (populated from the provider's `/models` response or the config override) and ignores `max_output_tokens` entirely. The 80% default trigger point is unchanged — only what it measures against. Long-context model users will notice sessions running dramatically longer before compaction.

`/compact why` now prints the threshold calculation so you can verify it's using the right window size.

`maybe_auto_compact` emits a new OTel span `anvil.autocompact.threshold` with attributes `context_window`, `used_tokens`, `threshold_pct`, and `triggered` so threshold behavior is auditable from telemetry.

## Mouse Capture Default-OFF (#696 P4)

Mouse capture is now disabled by default on all platforms, matching the principle established in `feedback-cross-platform-ux-defaults.md`. The previous default-ON behavior broke terminal copy-paste (Cmd+C / Ctrl+Shift+C / Ctrl+C depending on terminal and OS) for users who hadn't explicitly configured mouse support.

A toast appears on first run: "Mouse capture is OFF. Enable with `/config mouse_capture true` or `--mouse` flag for scroll and click support." The toast is one-time (suppressed after acknowledgment via `mouse_capture_toast_seen: true` in config).

The regression test `mouse_capture_default_off_regression` guards this — it asserts `TuiConfig::default().mouse_capture == false` at the type level so a future default change can't silently regress.

## Wizard Polish (#685)

Bracketed paste now works correctly inside textarea modals (the multi-line input component used in `/mcp builder` and other wizard steps). Previously, pasting a multi-line block into a textarea modal would either collapse to a single line or corrupt the cursor position depending on terminal.

The fix wires the existing `handle_paste` logic (from `tui::paste`) into the `TextareaModal` event loop. The same bracketed-paste detection and `\r\n` → `\n` normalization that works in the main input box now applies to all textarea fields.

## TUI Fixes

**MemorySnapshot rail parity (#695).** `MemorySnapshot` rendering inside the vertical-split rail now uses `layouts::common` helpers rather than a hand-rolled draw path. The snapshot renders at the same fidelity in the rail as in the classic inline view.

**Per-tab relay routing (#696).** `relay::user_message` and `relay::slash_result` route to the active `Tab.id` rather than broadcasting. This prevents responses from tab A appearing in tab B's scrollback when two tabs have concurrent inference in flight.

**OAuth / local / cloud cost label (#696 P1).** The TUI status footer's cost display now shows a semantic label (OAuth, local, or cloud) instead of a fabricated dollar amount for provider types where per-token cost is not knowable.

**Alt-screen raw mode restore (#688).** `restore_alt_screen` re-enables raw mode after returning from an inline operation. This was the root cause of the "keyboard stops working after `/mcp builder` cancel" bug that affected v2.2.17.

**Force-full-redraw consumption (#688).** The `FORCE_FULL_REDRAW` event is now consumed inside `handle_repl_command_tui` so the blank-screen-after-cancel regression introduced in v2.2.17 does not recur.

**Mouse capture + alt-screen pairing (#688).** Mouse capture state is now paired with alt-screen state — enabling mouse capture outside the alt-screen no longer leaves the terminal in an inconsistent state after exit.

**Force full redraw after inline-op restore (#687).** After any inline operation restores the alt-screen, a full redraw is forced. Partial-frame artifacts from operations that exit mid-render are no longer visible.

**Textarea keybinds (#686).** `Enter` submits in textarea modals; `Ctrl+N` inserts a newline. Previously the assignment was inverted, making single-line submission require `Ctrl+N`.

**`/mcp builder` long-description textarea (#684).** The MCP builder's long-description field is now a multi-line textarea modal rather than a single-line input.

## Release-Pipeline Hardening (#654)

Phase 6 of `release.sh` (SSH-based deploy steps) now guards every remote call against `set -e` silent-exit. Previously, a failed SSH hop in Phase 6 could leave the pipeline in a state where subsequent steps ran against a stale remote. The fix wraps every SSH call in an explicit `|| { echo "Phase 6 SSH failed: ..."; exit 1; }` guard so failures surface immediately.

## PermissionPrompt Regression Test (#677)

End-to-end round-trip test for `PermissionPrompt`: the test fires a tool call that requires a permission prompt, verifies the prompt is rendered, sends the approval event, and asserts the turn completes. This guards against the class of bug where permission prompt state can desync from the turn loop.

## Compatibility

v2.2.18 is binary-compatible with v2.2.17 sessions. `anvil --continue` and `anvil --resume <id>` work across the upgrade.

New config keys (`mouse_capture`, `mouse_capture_toast_seen`) are optional. Existing configs continue to parse.

The autocompact fix is transparent — no configuration change required. Sessions that were over-compacting on long-context models will automatically use the correct threshold after upgrade.

## Install

```bash
# macOS / Linux:
curl -fsSL https://anvilhub.culpur.net/install.sh | sh

# Homebrew:
brew upgrade culpur/anvil/anvil

# Windows / FreeBSD / NetBSD:
# Download from https://github.com/culpur/anvil/releases/tag/v2.2.18
```

## Commit Log

| SHA | Scope | Summary |
|-----|-------|---------|
| `240b778` | test(#677) | End-to-end PermissionPrompt round-trip regression |
| `603d98a` | tui(#697) CRITICAL | Autocompact threshold uses context window, not output cap |
| `fe65738` | tui(#696 P5) | Instrument `maybe_auto_compact` + `/compact why` |
| `53d0be4` | tui(#696 P4) | Mouse capture toast + default-OFF regression test |
| `e1cb410` | wizard(#685) | Bracketed paste in textarea modal |
| `d28230c` | fix(release) | Guard Phase 6 SSH calls against `set -e` silent exit (#654) |
| `d593576` | tui(#695) | `MemorySnapshot` uses `layouts::common` helpers (rail parity) |
| `5f084aa` | main(#696) | Per-tab routing for relay `user_message` + `slash_result` uses active `Tab.id` |
| `c07e997` | tui+relay(#696) | `tab_id` stable IDs + drop bad `paired_count` gate |
| `06d2446` | tui(#696) | `/tab new/rename/switch` + `Ctrl+T` broadcast to relay |
| `cc3aaa5` | tui(#696 P1) | OAuth/local/cloud cost label, not fake dollar figure |
| `91bf8c1` | #696 | `MemorySnapshot` relay event + `SessionMeta context_max/build_sha` |
| `7991e46` | relay | Add `cost_type` + layout to `SessionMeta`; `push_system` forwards to relay |
| `5bcf65f` | tui(#688) | Re-enable raw mode in `restore_alt_screen` |
| `c3b3260` | fix(tui) | Consume `FORCE_FULL_REDRAW` in `handle_repl_command_tui` |
| `6e345c6` | tui(#688) CRITICAL | Pair mouse-capture state with alt-screen state |
| `6b125a8` | tui(#687) | Force full redraw after inline-op alt-screen restore |
| `3d0744b` | textarea(#686) | Swap keybinds — `Enter` submits, `Ctrl+N` newline |
| `4978f1a` | mcp-builder(#684) | Multi-line textarea modal for long-description fields |
| `cb2c3c7` | viewer(#683) | Always-visible slash bar in header + `Cmd+K` palette |

**passage repo:**

| SHA | Summary |
|-----|---------|
| `f2bce1d` | viewer(#692): `cost_type` chip in footer + collapsible tool cards (#695) |
| `e5cf35e` | viewer: default layout = `vertical_split + tabs` |
| `5d82dae` | relay+viewer: default-allow forwarding + slash completion on every input |
| `2d06753` | relay+viewer(#696): `tab_opened` dedup, rail rename, drop bad pairing shortcut |
| `ee49cde` | viewer(#696): rail/secondary `+ new session` bypasses picker |
| `04a2fb2` | relay(#696): cache + broadcast `memory_snapshot` |
| `d6f38c2` | viewer(#696): OAuth cost label + tab X/+ in secondary bars + rail close |
| `5a82463` | viewer(#696): consume `memory_snapshot` event from TUI |
| `56b817c` | viewer(rail): split rail into top + bottom groups with flex spacer |
| `c11c324` | viewer: `/configure` + gear open panel in active skeleton |
