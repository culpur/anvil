# Anvil v2.2.17 — The Setup Wizard, Reflection, Sandboxing, and the Source Viewer

Released: 2026-05-18

v2.2.17 is the **first-impressions release**. The setup wizard is now a single continuous in-TUI experience from "Welcome to Anvil" through to your first prompt — no inline stdin prompts, no dropping back to shell mode, no flicker. Underneath that polish: Anvil now notices when it's stuck and pivots (autonomous reflection), AnvilHub's GitHub-style source viewer is finally end-to-end across 558 packages, and the hub-install pipeline gets sandbox detonation before any package touches your machine. 30+ commits over v2.2.16.

## Headline: The new setup wizard

First-run setup used to be 6 inline stdin prompts followed by 2 modal screens. v2.2.17 unifies all 9 steps inside a single ratatui alt-screen session — one screen mode enter, one screen mode exit, banner descriptions between every step, and a zero-seam handoff into the main TUI.

- **Welcome card** with tagline + "Let's get you setup."
- **Step 1 — Vault** — ConfirmModal + PasswordModal × 2 (set + confirm, retry loop on mismatch)
- **Step 2 — Provider** — ChoiceModal with the 35-provider catalog (top picks + "More providers..." overflow)
- **Step 3 — Sign In** — branches by provider. Anthropic OAuth runs **inside the wizard alt-screen** via the reusable `OAuthFlow` state machine. Browser opens, callback polls every 100ms, success card appears without a keypress, elapsed counter ticks every second. Or paste an API key via PasswordModal. Or skip for local Ollama.
- **Step 4 — Default Model** — ChoiceModal from the live provider /models list
- **Step 5 — Ollama Endpoint** — TextInputModal (new modal type — single-line input with ghost default)
- **Step 6 — Profile** — TextInputModal
- **Steps 7 + 8 — TUI Layout + Preferences** — the polished flow from v2.2.16
- **Step 9 — Claude Code migration** — detects `~/.claude/`, ConfirmModal to import sessions/skills/plugins/memory inline
- **Done** banner → alt-screen drops → vault unlocks with the password you just set (no second prompt) → main TUI takes over

Returning users with an existing vault get a minimal alt-screen wrapper containing only a PasswordModal — no more inline stdin echo for vault unlock either.

Per-step contrast bumped from `Modifier::DIM` to `Color::Rgb(170,170,170)` (luminance 0.67 vs. the old 0.40) — secondary text is now readable on the modal footers. `ANVIL_CONFIG_HOME` is honored by every wizard gate, vault path, and config write — `ANVIL_CONFIG_HOME=$(mktemp -d) anvil` now correctly triggers a fresh first-run without touching your real profile.

## Autonomous reflection loop

Anvil has always **survived** failures — retries on 429, stream-stall fallback to non-streaming, OOM refusal with quant suggestions, OAuth token keep-alive, reactive compaction on context overflow. But survival isn't reflection: if Anvil ran the same failing tool three times in a row, it kept trying.

v2.2.17 adds in-session reflection across three orthogonal components, wired through the runtime's eight-axis capability contract:

- **`StuckDetector`** observes every tool outcome. Four patterns trigger detection: `ToolLoop` (same tool + args + error ≥3×), `Thrashing` (>50% error rate over last 5 calls), `InferenceStall` (>600s of inference without progress), `Oscillation` (same file edited ≥3× in one turn). Window capped at 20 events.
- **Per-turn scratchpad** records every failed `Tool` outcome as a `FailedAttempt { tool, args_summary, error_summary }`. On the next inference call within the same turn, the scratchpad is prepended as a `<system-reminder>` block: *"Previously tried in this turn (avoid repeating): ..."*
- **Strategy reminder** fires when a pattern is detected. Before the next inference call, Anvil injects: *"Stuck pattern detected: \<pattern\>. Things tried that didn't work: ... Step back. Reason about WHY. You have permission to abandon the current path."* Quiet window prevents reminder-spam: silent for N turns after firing.

`/reflect` exposes the loop manually — `/reflect status` shows the detector window, `/reflect window` lists the last 20 events, `/reflect scratchpad` dumps current attempts. Five OTel events surface telemetry: `reflection.stuck_detected`, `reflection.strategy_switch`, `reflection.scratchpad_pushed`, `reflection.scratchpad_size`, `reflection.user_invoked`.

Configurable via `settings.json`:
```json
{
  "reflection": {
    "enabled": true,
    "stuck_threshold_calls": 3,
    "stall_timeout_secs": 600,
    "quiet_window_turns": 3
  }
}
```

The full 8-axis wiring lives in `crates/runtime/src/reflection/`, with the slash-command drift gate covering enum + parser + spec + dispatch. `turn_executor::run_turn_inner` now auto-feeds every tool outcome — no callsite hook required.

## anvil-sandbox-runner: detonation before install

`anvil-sandbox-runner` is a new binary that ships alongside `anvil` in every release archive (7 platforms × 2 binaries = 14 release artifacts). When `anvil skill install <slug>` is invoked, Anvil first runs the package's `installCmd` inside a sandbox, captures stdout/stderr/files-written/exit-code, and shows you a detonation report **before** the real install touches your filesystem.

Per-OS backends:
- **Linux**: `unshare --user --map-root-user --mount --ipc --pid --uts --fork [--net]`
- **macOS**: `sandbox-exec` with deny-default profile + sandbox-root file-write allow
- **Windows**: `cmd /C` with sandbox-rooted CWD + `TMP`/`TEMP`/`USERPROFILE` env vars + post-run filesystem diff
- **Other Unix**: `sh -lc` with file-diff escape detection

Output is JSON: `{ exit_code, files_written, files_written_outside_sandbox, stdout_tail, stderr_tail, duration_ms, killed_by_timeout, sandbox_backend, sandbox_root, allow_network }`.

Flags: `--timeout=N` (default 60s), `--allow-network` (deny by default), `--version`, `--help`. Integration tests cover benign installs, malicious writes outside sandbox root, missing-arg validation, and non-zero-exit surfacing.

Companion: `PluginMetadata.hub_trust_level` now reads from `~/.anvil/hub-registry.json` at load time. `/plugin list` renders `[verified]`, `[culpur-official]`, or `[REVOKED]` badges next to plugins based on the live registry — post-install revocations surface immediately without re-querying AnvilHub.

## AnvilHub: source viewer end-to-end

The marketplace gets its long-promised GitHub-style file browser.

**Backend (passage):**
- New routes `GET /v1/hub/packages/:slug/source` (file tree) and `GET /v1/hub/packages/:slug/source/:filepath(*)` (UTF-8 content with MIME + path-traversal guards). LRU cache: 50 packages × 1h TTL.
- New IAM user `anvilhub-source-writer` on `s3.culpur.net` with bucket-scoped write to `anvilhub-source` (versioning enabled).
- New `HubPackage.sourceArchiveUrl` / `sourceArchiveSize` / `sourceArchiveSha256` columns.
- All MinIO credentials eyaml-encrypted in Puppet Hiera; pass through to passage `.env` via `passage_app::minio_*` params.

**Data:**
- **All 558 packages** have synthesized source archives (per-type layouts: SKILL gets README.md + SKILL.md + install.sh + examples/ + manifest.json; PLUGIN adds plugin.toml + src/main.rs stub; AGENT adds agent.md + traits.toml; THEME adds theme.toml + preview.md). Pure deterministic synthesis from existing DB columns — no LLM, no fabricated function bodies.
- **All 558 packages** have synthesized `readme` markdown so the detail-page Documentation block renders for every type. SKILL/AGENT/PLUGIN/THEME each get a type-appropriate readme shape including Installation, Tags/Capabilities, Categories, and Details sections.

**Frontend (anvilhub-web):**
- `<SourceBrowser>` component: file tree left, Shiki-highlighted content right, on-demand grammar loading, IndexedDB cache (`idb-keyval`, 24h TTL, keyed by slug+version+path).
- `<TrustScoreBadge>` shows Verified Publisher / Verified Build / Featured states inline with the package meta card.
- Tab control on each detail page: `[Documentation] [Source]`.
- All 4 type routes — `/skills/[slug]`, `/agents/[slug]`, `/plugins/[slug]`, `/themes/[slug]` — render the source browser uniformly.

## Skill-chain builder (Feature 2)

Anvil v2.2.16's `/chain` executor gets the publishing side it always needed:

- New Prisma models on passage: `HubUser`, `HubPublisher`, `HubUserDraft`, `HubChain` + `PackageType::SKILL_CHAIN` enum value.
- New routes: `POST /v1/hub/drafts` (create/update), `GET /v1/hub/drafts` (list), `POST /v1/hub/chains/publish` (validate + persist with DAG cycle check + topological sort), `GET /v1/hub/chains/:slug` (public read).
- `SKILL.md` frontmatter extended with `inputs:` / `outputs:` blocks. Each slot has `name` (snake_case ≤32 chars), `kind` (Text/File/Json/Image/Boolean, default Text), `description`, `required` (default true). Cross-list duplicate names allowed; intra-list rejected.
- passage surfaces `manifest.inputs` / `manifest.outputs` in package JSON when SKILL.md is in the source archive.
- anvilhub-web `/builder` React Flow canvas (D) and `/my-chains` + `anvil://` deep-link install button (E) — full skill-chain composition pipeline.

## TUI rendering: region-targeted partial repaints

Building on the photosensitivity fix in v2.2.16 (#622 + #629), v2.2.17 makes the redraw scheduler region-aware:

- `DirtyRegions` enum extended: `HEADER`, `INPUT`, `RAIL`, `STATUS`, `OVERLAY`, `SCROLLBACK`, `CHROME`, `TAB_STRIP`, `AGENT_PANEL` bits
- Each layout (`classic`, `vertical_split`, `journal`) gates its per-area `render_widget` calls on the matching bit. Narrow dirty sets (e.g. `HEADER` only) skip non-matching bands; ratatui's cell diff keeps unchanged pixels.
- First-frame after layout-switch, resize, or modal-close: `DirtyRegions::ALL` ensures full paint. Subsequent frames are surgical.
- 12 new tests guard the per-region contract: `classic_renders_only_header_on_header_dirty`, `vertical_split_renders_only_rail_on_rail_dirty`, `journal_redraws_only_body_on_scrollback_dirty`, etc.
- Click hit-test geometry refreshed every frame even when strip pixels don't repaint — `rebuild_tab_hits` and `rebuild_thread_switcher_hits` decoupled from paint.

The photosensitivity gate is preserved: no full-screen `Clear` widgets without `DirtyRegions::ALL` set.

## TUI bug fixes

- **OAuth callback responsiveness (#582)**: OAuth completion channel now polled at the top of every `read_input` frame tick. Login completes without requiring a keypress.
- **Journal layout ghosting (#583)**: Header + input rows now use sub-region `Clear` widgets so stale cells don't survive scrollback-only dirty frames.
- **Rail keybinds wired (#634)**: `g` / `d` / `s` / `a` / `Ctrl+R` for deck/tools/sessions/agents/deck-cycle, gated on empty input buffer + no modal + completion-popup closed + live scrollback. Plus a drift test that asserts every advertised key has a handler — caught the original gap.
- **Wizard single alt-screen session (#579)**: First-run wizard's modal-driven steps now run inside one persistent alt-screen session. No more 4× screen wipes between layout/mouse/theme/permission prompts.
- **Session auto-titling (#580)**: `derive_title_from_first_message` now actually fires. First user message becomes the session title (40-char truncated, slash-commands stripped, sidecar-safe alphabet).
- **`/rewind` slash command (#557)**: Full new command with TUI picker. Optional "Summarize up to here" action force-compacts the prefix into a Summary message and continues from there. 5 OTel events.
- **terminalSequence hook output (#556)**: Hooks can now return `{"terminalSequence": "\x07"}` to emit BEL / OSC 9 / OSC 777. Disallowed sequences (full-screen clears, arbitrary CSI) are rejected with a warning.

## Reactive compaction and Stop hook

- **Reactive compaction (#564)**: Providers (Anthropic, OpenAI/Azure/Copilot/Cursor, Ollama) now detect context-overflow errors and return `RuntimeError::ContextTooLong { overflow_tokens }`. Turn loop catches, runs `compact_session_reactive` (targets half of remaining after overflow), and retries the same turn — user input not lost. Configurable via `compaction.reactive_enabled` and `compaction.reactive_max_retries` (default 1).
- **`HookEvent::Stop` (#566)**: Stop hooks fire at end-of-turn when the assistant message has no `tool_use`. Block decisions inject the reason as a synthetic user-text message. New env `ANVIL_STOP_HOOK_BLOCK_CAP` caps how many times Stop hooks can hold a turn (default 5) to prevent infinite loops.

## Session and compaction polish

- **Per-tab autocompact threshold isolation (#560)**: Tab A's high-context autocompact no longer affects Tab B's threshold calculation. Threshold reads per-call from each tab's `ConversationRuntime`.
- **Hook paths after EnterWorktree (#561)**: After CWD swap, hook paths re-resolve against the new workspace root. Project-local hooks no longer point at stale paths.
- **NO_COLOR / FORCE_COLOR in settings.env (#567)**: TUI always renders with colors regardless of env. The env vars only affect headless `--print` output. Fixes accidental color-stripping when wider toolchains set `NO_COLOR=1`.
- **Release surface manifest fix (#621)**: `release-surfaces.yaml::provider_count_invariant` now references `provider_display_name` (the actual function) instead of the non-existent `provider_label`. `verify-release-surfaces.sh --self-test` 11/11 passing.

## Quality bar

- **Tests**: runtime 1106 (+51 from v2.2.16), anvil-cli 717 (+12), commands 235 (+5), api 330. All green.
- **Clippy gate**: `#![deny(clippy::print_stdout, clippy::print_stderr)]` continues to enforce no-stdout-in-TUI across every TUI-touching file. No regressions.
- **Drift gates**: slash-command bidirectional drift test (`every_slash_command_variant_has_a_spec`) updated to spec count 119; rail-keybinds drift test (`every_advertised_rail_key_has_a_handler`) blocks future regressions.
- **Worktree isolation**: parallel agent work used isolated `git worktree` checkouts to prevent the working-tree clobbering that bit one early v2.2.17 finisher. New SOP for multi-agent v2.2.x runs.
- **Honest progress reporting**: per `feedback-no-silent-deferral.md`, agents now report partial progress with permission requests rather than silently deferring. Multiple v2.2.17 deliverables surfaced honest blockers that turned into proper follow-up scope rather than fake-completion drift.

## Cross-fleet ops landed in this cycle

Out of band but worth noting — Anvil's release pipeline now depends on:

- **passage_prod role rename**: `passage_dev` → `passage_prod` on the HA pair (f0/f1/f2), 63 tables re-owned, replication healthy.
- **Patroni replication recovery**: silent replication death since ~2026-05-08 (JMicron SATA incident era) detected and resolved via `patronictl reinit` on f1 + f2.
- **MinIO env wired into passage prod** via Puppet (`passage_app::minio_*` params, eyaml secrets).

## Compatibility

- Anvil v2.2.17 is binary-compatible with v2.2.16 sessions. `anvil --continue` and `anvil --resume <id>` work across the upgrade.
- New `settings.json` fields are optional. Existing configs continue to parse.
- The reflection loop is on by default. Disable via `{"reflection": {"enabled": false}}` if you want pre-v2.2.17 behavior.

## Install

```bash
# macOS / Linux:
curl -fsSL https://anvilhub.culpur.net/install.sh | sh

# Homebrew:
brew upgrade culpur/anvil/anvil

# Windows / FreeBSD / NetBSD:
# Download from https://github.com/culpur/anvil/releases/tag/v2.2.17
```

## Acknowledgments

v2.2.17 was implemented through coordinated parallel agent runs in isolated worktrees, with honest blocker-reporting (per `feedback-no-silent-deferral.md`) replacing silent-deferral patterns. The "no punts to v2.2.18" scope policy meant every flagged deferral got finished — `HookEvent::Stop` plumbing, `/rewind` from scratch, reactive compaction across 7 providers, wizard alt-screen refactor, F1 frontend, and the full F2 backend all landed in this cycle rather than slipping.

—

🤖 Anvil v2.2.17 — reflect, sandbox, view source.
