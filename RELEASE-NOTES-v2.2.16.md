# Anvil v2.2.16 — The TUI Layout System

Released: 2026-05-17

v2.2.16 is the visual-architecture release. Anvil's terminal UI was a single hard-coded draw path for its entire history; with v2.2.16 the renderer is data-driven and ships **eight live-switchable layout variants** built on a per-tab `TuiLayoutConfig`. Fresh installs land on **Vertical Split + Tabs** — a persistent left rail of sessions next to a swappable right deck — and `/layout <kind>` swaps the renderer without restarting Anvil. 50 commits over v2.2.15.

## Headline: TUI Layout System

Four layout architectures × tabs/no-tabs = eight variants, every one of them a real renderer, not a mockup:

- **Vertical Split** (Layout A / A1 with tabs) — persistent left rail showing sessions, agents, tools, MEMORY, gate state; swappable right deck for chat or whatever the active subcommand renders. **The new default.** Rail owns all chrome (banner, status, model, cost), deck has input only. Split-anchor at the rail edge is mouse-draggable.
- **Classic** (Layout D / D0 with tabs) — single-deck rendering, pixel-identical to pre-v2.2.16. The inline 7-layer MEMORY block sits directly above the input. This is what existing users had; it's still here, one `/layout classic-tabs` away.
- **Three-Pane** (Layout B / B1) — FOCUS / LOG / CONTEXT bands with an always-on input row. No vim-modal — Insert mode is gone; typing always edits the input, with a framed hint + ghost text making the active band discoverable.
- **Journal** (Layout C / C1) — timestamped single-column scroll for when you want a log-only view. Ctrl-K opens a command palette. Header and input rows get a `bg_primary` clear-before-draw pass so no stale cells haunt the chrome.

### `/layout` command

`/layout` is a full eight-axis capability — definition, registration, completion, handler, dispatch, rendering, gate, OTel + tests — wired through the unified `commands/dispatch.rs::dispatch_slash_command`:

- `/layout list` — tabular view of the eight variants with current selection highlighted
- `/layout <alias>` — switch the active tab's layout immediately. Aliases: `classic`, `classic-tabs`, `vertical-split`, `vertical-split-tabs`, `three-pane`, `three-pane-tabs`, `journal`, `journal-tabs`
- `/layout <alias> --global` — write the choice to `~/.anvil/config.json` so new tabs and future runs inherit it
- `/layout reset` — back to the default (`vertical-split-tabs` in v2.2.16)

The state-machine contract is enforced by integration tests: switching layouts resets the per-tab `LayoutLocalState` to the new kind's defaults, shared session state (log, input buffer, model name) survives the transition, and switching to the same config is a no-op (equal-guard short-circuit). Live-switch fires a terminal clear so the previous layout's cells don't bleed through. The slash-command popup is wired into all six render paths (Vertical Split + Classic + Three-Pane + Journal × tabs/no-tabs) so completion works regardless of which renderer is active.

### Settings schema and migration

`tui_layout` lands as a first-class config block in `~/.anvil/config.json`:

```json
{
  "tui_layout": { "kind": "vertical-split", "tabs": true },
  "tui_layout_intro_seen": false
}
```

Or the short-form alias:

```json
{ "tui_layout": "vertical-split-tabs" }
```

**Migration safety.** `parse_optional_tui_layout_config` only falls back to the new VerticalSplit+tabs default when the `tui_layout` key is **absent** from the merged config. Users who set an explicit value (including the v2.2.14/v2.2.15 layout-preview cohort) keep their existing choice — a new `tui_layout_explicit_classic_in_settings_is_preserved` unit test guards this contract. First launch after upgrade emits a one-time toast that surfaces the new default and tells you `/layout classic-tabs` restores pre-v2.2.16 rendering; the toast suppresses itself via `tui_layout_intro_seen: true`.

### Wizard step 7

The first-run setup wizard now defaults Step 7 to Vertical Split (option `[1]`). Classic moves to `[2]`; Three-Pane and Journal stay at `[3]` and `[4]`. New installs that just press Enter four times will land on the new visual default with tabs enabled.

## TUI correctness — the long tail

The layout work bubbled up a lot of cross-cutting redraw and input issues. All of them are fixed:

- **Slash-completion popup** (#BUG-6) — wired into every render path, not just classic
- **Per-keystroke redraw** (#BUG-5) — input renders live across all layouts; previously some renderers only redrew on a tick
- **Three-pane Insert discoverability** (#BUG-4) — framed hint + ghost input makes the always-on input legible; the CONTEXT band uses `Constraint::Fill` so it actually fills available height
- **Layout-switch ghosting** (#bug-3) — terminal clear on `/layout` switch drops stale cells; Journal header/input rows clear-before-draw to prevent ghosting at the top edge
- **Repaint after inline flows** (#bug-2) — force-full-repaint after the OAuth and setup flows return control to the TUI
- **OAuth callback responsiveness** — the background callback channel is polled each frame, so login completes without requiring a keypress
- **Vertical-split rail polish** (#594, #596, #602) — rail owns banner + gate banner + status + model + cost (deck has only input), split-anchor draggable at the rail edge, tool-call boxes close cleanly, markdown styled, cost rendered to 2 decimals, uppercase section headers, cross-tab status aggregation, QMD folded into the MEMORY block, agent tab-binding

## Paste handling

Paste is now first-class across multiple paths (#599 / #601 / #604):

- **Consolidated paste handler** — one handler routes terminal bracketed paste, OSC52, and drag-and-drop file paths. Mouse capture is off by default so native terminal copy works; clicking is opt-in via `mouse: true` in settings.
- **Document content blocks** — `ContentBlock::Document` is wired end-to-end for PDF and Office docs. Dropping a `.pdf` into the input attaches it as a document block to the next request; the provider gets the file inline, not a hallucinated transcript.
- **Long-paste placeholder** — submit-time path detection collapses multi-KB pastes to a `[Pasted 4,127 chars]` placeholder in the rendered history while the full content goes to the provider.
- **Keystroke-burst detection** — drag-and-drop on terminals that don't emit OSC52 is detected by inter-keystroke timing and converted to a paste, not 200 individual key events.

## Cancel correctness

`Ctrl+C` mid-stream now actually aborts the in-flight HTTP read across **all 7 providers** (#605 / #606):

- `DefaultRuntimeClient` honors the cancel token in every provider implementation (was wired in some, no-op in others)
- `tokio::select!` on the cancel token wraps the blocking HTTP read so the connection actually closes when the token fires — previously the request would complete to EOF before noticing the cancel
- New wiremock integration test exercises the cancel path end-to-end

## v2.2.16 correctness work

| Area | Fix |
| --- | --- |
| Vault retry | `/vault unlock` retries up to 3 times and pre-fills the prompt on failure (#bug-1) |
| Welcome banner | Names the active provider, not hardcoded "Anthropic" (#562) |
| Session titles | Heuristic skips a bare URL as the first message (#563) |
| 5xx error names | Errors name the configured provider/gateway, never hardcoded Anthropic URL (#568) |
| Spinner color | Warm green → amber → red gradient based on elapsed seconds (#558) |
| Read tool offset | Accepts string forms with whitespace and `+` prefix (#555) |
| MCP tool timeout | `ANVIL_MCP_TOOL_TIMEOUT` env override per request (#559) |
| Model fetch | Async `fetch_all_configured_models` with timeout + Ctrl+C cancel (#BUG-7) |
| OAuth parser | Strict RFC 6749 token-exchange parser + startup validator (#595, BUG-14) |
| OAuth lockout | Lenient scopes deserializer prevents auth lockout on tokens that drift from the strict shape (#565) |
| AnvilHub gate | Verification gate, `/hub status` (all 8 axes), `/plugin update REVOKED` guard |
| AnvilHub schema | `HubPackage` verified-badge structs + `require_verified` config |
| Update probe | Prefers anvilhub `/api/version`, falls back to GitHub Releases |
| Provider login UX | `ProviderLoginModal` for in-TUI OAuth/API-key flows (#578); `/login` and `/provider login` intercept and open the modal |

## Test coverage

| Crate | Tests | Delta |
| --- | --- | --- |
| runtime | 1003 | +10 (TuiLayoutConfig parse/default/intro suite) |
| anvil-cli (live-switch) | 10 | +10 (new test file) |
| Workspace total | green | 0 failures across all crates |

## Surfaces touched

Every public surface is updated in lockstep with this release:

- `RELEASE-NOTES-v2.2.16.md` at repo root (this file)
- GitHub Release `v2.2.16` on `culpur/anvil`, seven platform binaries
- Public `culpur/anvil` README: `## Changelog` gets a `### v2.2.16` block appended on top — earlier entries byte-immutable
- AnvilHub `/about`, `/install`, homepage: version strings and changelog entry
- anvilhub `/api/version`: `version: "2.2.16"`, `releasedAt` set to release ISO timestamp
- Homebrew formula `culpur/homebrew-anvil`: version + SHA256 for both Darwin arches
- culpur.net/anvil (WordPress page 619): install CTAs and "Also in the box" feature card
- Launch article on culpur.net introducing the TUI layout system

## Upgrade

```
brew upgrade anvil    # macOS / Linux
```

Or download the binary for your platform from the GitHub Release. `anvil --version` will report `2.2.16`.

**What you'll see on first run after upgrade:**

- If you never set `tui_layout` in your config, you land on **Vertical Split + Tabs** and get a one-time toast explaining the change. `/layout classic-tabs` brings back the previous rendering immediately.
- If you set `tui_layout` explicitly any time during the v2.2.16 preview cycle, your choice is preserved — no toast, no surprise.

## What's next

v2.2.17 will fold the routines daemon and seven-layer memory promotion patches that didn't make this release's gate. The TUI Layout System is the foundation for the v2.3 deck-extension API — third-party deck panes will register against the same `TuiLayout` dispatch that powers `vertical-split` today.
