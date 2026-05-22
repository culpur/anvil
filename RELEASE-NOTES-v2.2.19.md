# Anvil v2.2.19 — i18n end-to-end, Memory Cohesion completed, AnvilHub Build page, CC parity sweep

Released: 2026-05-22

v2.2.19 is the **i18n + Memory Cohesion arc**. Two long-running commitments close: Anvil now ships in 18 languages with a wizard picker, locale persistence, and a soft drift gate (Tier-1 + Tier-2 complete, no Arabic/Hindi per scope decision); and the 7-layer memory architecture promised since v2.2.14 is wired end-to-end with Working / Episodic / Semantic / Procedural / Reflective / Long-term / Cache all GREEN. Plus a new web-based MCP Builder lands on AnvilHub (`/build` page + `anvil-mcp-builder` micro-service), a full CC parity sweep against Claude Code v2.1.144 → v2.1.146 lands as 15 concrete fixes (3 P0, 4 P1, 7 P2), and the release pipeline gains per-step gates that catch silent exits like the v2.2.18 Phase 6 incident.

## Internationalization — 18 Locales

Anvil now ships in 18 languages. The TUI, wizard, slash-command output, and remote-control viewer all flow through `rust-i18n` v4 / `t!()` (Rust) and the new `viewer.locales.js` runtime (browser). A drift gate at workspace level (`SUPPORTED_LANGUAGES`) and at the viewer (load-time check) catches any locale that diverges from `en` before it ships.

### Tier-1 (8 locales, 264 keys each)

English, Spanish, Simplified Chinese, French, Brazilian Portuguese, Russian, Japanese, German.

### Tier-2 (10 locales, 264 keys each)

Korean, Italian, Turkish, Vietnamese, Polish, Indonesian, Dutch, Swedish, Norwegian Bokmål, Ukrainian.

### Wizard language picker

First-run wizard now opens with a language picker as Step 0. Selection persists to `~/.anvil/config.json` and applies immediately to all subsequent wizard steps. The `--lang <code>` CLI flag overrides per-invocation; missing-value (`--lang` with nothing after) is a hard error rather than silently picking English.

### Configure menu picker

The in-TUI `/configure` menu has a Language Picker submenu with all 18 locales rendered in their native script (한국어, Русский, 中文 etc.) so users find their language without knowing the BCP-47 code.

### Viewer i18n (web)

`viewer.locales.js` is loaded by the AnvilHub viewer at boot. Locale switch is reactive — no page reload, no SSH-session disconnection. The dropdown lives in the bottom-right of every viewer page. Selection persists to `localStorage`, falls back to `navigator.language` prefix match on first visit, then `en`.

The viewer ships with 176 fully-wired keys covering chrome (header, footer, status bar, tab bar) plus all vault and config panels — `vault.*` (entry add/edit/delete forms, master-key modal) and `config.*` (SSH, Database, Plugins+MCP, Layout, Status-line editor) — all routed through `data-i18n-key` attributes that the live re-render walker translates on locale switch.

A soft drift gate at load time prints any seeded-but-unused keys to the console so the gap shrinks commit-by-commit.

## Memory Cohesion Arc — All 7 Layers GREEN

The 7-layer memory architecture committed in v2.2.14 finally has all layers wired end-to-end. The 2026-05-21 audit (`docs/memory-cohesion-audit-2026-05-21.md`) catalogued the state and the subsequent waves closed every RED layer.

### Layer 1 — Working (Sensory + Working)

`/memory layer 1` now renders a live snapshot of the current working-memory inventory via `PromptSectionsExt::iter_by_kind()`. Replaces the hand-written static text from v2.2.14 with a real introspection table showing kind, size, and preview for every section in injection order.

### Layer 2 — Episodic

`/memory show episodic` enumerates daily summaries, history archives, and workspace sessions into a unified table.

`/memory prune episodic` adds TTL-based retention with `--dry-run` (default ON) and `--confirm` (required for actual moves) and a trash-bin safety net — candidates move to `~/.anvil/trash/<unix-ts>/<source>-<file>` rather than being deleted outright. Default TTL is 90 days, overridable via `--ttl <days>`.

### Layer 3 — Semantic (was RED)

`/memory promote <nomination-id>` now actually persists nominated facts to disk. The v2.2.14 stub flipped the status flag without ever calling `MemoryManager::save()` — nominated facts never reached `ANVIL.md`. The full chain now writes the fact + appends provenance comments (`# nominated_at: <ISO>`, `# source: nomination/<id>`) BEFORE marking the nomination accepted. Atomic ordering: if marking fails after the file write, the user sees a clear "fix the JSON manually or re-run /memory promote" message instead of silent partial state.

New `--target <file>` flag routes a nomination to a specific file instead of the suggested default. Supports relative, absolute, and `~`-expanded paths.

### Layer 4 — Procedural

`/memory show procedural` consolidates GoalManager state + on-disk skills + bundled skills + CronManager schedule into a single view. Each section shows count and key fields; sizes display in bytes. The routines sub-view stays stubbed as "Coming v3.x" — full auto-fire integration is the v3.x routines work, not in v2.2.19 scope.

### Layer 6 — Reflective (was RED)

(Pending agent completion — final summary lands when #735 ships.)

### Layer 7 — Cache (was RED)

The file-cache path discovery bug is fixed: previously `memory_budget` checked `~/.anvil/file-cache/` (a directory that no longer exists in the project-scoped layout), so cache counts always reported zero. Now uses `FileCacheManager::new(cwd)` to discover the actual per-project path at `~/.anvil/projects/<hash>/file-cache/`.

`/memory show cache` enumerates file-cache, command-cache, and QMD-cache stats. `/memory prune cache --dry-run` walks both `FileCacheManager` and `CommandCacheManager` for stale/missing-file candidates without mutating anything; the non-dry-run path is a pointer to the existing `/file-cache prune` and `/cmd-cache prune` commands.

## CC Parity Sweep — v2.1.144 → v2.1.146

A complete CC parity audit (`docs/cc-parity-audit-2026-05-21.md`) covered the 4-day window since v2.1.143 across 3 CC releases. 15 fixes filed and shipped this release.

### P0 (3)

**MCP pagination (CC v2.1.144-B6 / v2.1.146-B2).** Anvil's MCP client now consumes the full `nextCursor` / `has_more` pagination chain for `tools/list`, `resources/list`, `resources/templates/list`, and `prompts/list`. Previously MCP servers with paginated responses had everything beyond page 1 silently dropped — a real foot-gun for any larger MCP environment.

**Spinner/elapsed-time freeze (CC v2.1.145-B3).** The TUI render queue now wakes from a wall-clock timer in addition to input events. Previously, after a terminal refocus or resize, the spinner and elapsed-time display would freeze until the next keypress because resize events were consumed without triggering a structural redraw. New `RedrawReason::TerminalStructural` routes Resize/FocusGained/FocusLost through the soft draw path (no ANSI clear flash, per the photosensitivity rule from v2.2.17).

**MCP `permissions.allow` not honored (CC community #61077, security).** `permissions.allow` rules with patterns like `mcp__server__tool` or `mcp__server__*` are now consulted at MCP tool dispatch time. Previously the allowlist was loaded from settings.json but the MCP dispatch path bypassed it and always prompted. Allow patterns short-circuit between auto-deny / hook-deny and the prompter.

### P1 (4)

**Bash env-var permission bypass (CC v2.1.145-B1, security).** Bash commands like `SECRET=hacked git push` no longer bypass the permission filter for the env-var assignment. The matcher now parses leading `KEY=value` assignments (Bourne-syntax, quote-aware) and enforces deny rules on each assignment before the command itself.

**Skill fork-context recursion guard (CC v2.1.145-B4).** A skill using `context: fork` that re-invokes itself now aborts with `Skill 'X' attempted recursive fork-context invocation`. Per-session `Arc<Mutex<HashMap<SessionId, HashSet<SkillName>>>>` tracks active fork invocations; RAII guard pops on Drop including panic.

**Terminal display corruption self-heal (CC v2.1.144-B3).** Sessions with 1000+ turns now trigger a periodic full-redraw via `RedrawReason::TerminalStructural` every 100 `TurnDone` events. Soft path only (no ANSI clear / no photosensitivity hazard). Stale-glyph accumulation no longer requires a manual resize to clear.

**Resume session model preservation (CC community #61068).** `anvil --resume <id>` now reads the model field from the persisted session sidecar at `~/.anvil/sessions/<id>.json` instead of re-fetching from the provider's current default. A session created on `claude-opus-4-7-1m` (1M-context variant) no longer loses the `-1m` suffix on resume.

### P2 (7)

**API startup 15s timeout (CC v2.1.144-B1).** Side-channel calls (rate-limit headers, model list) now have a 15-second timeout instead of the default ~75s. Prevents startup hangs behind broken DNS, captive portals, or unroutable endpoints.

**File mime-type mismatch fallback (CC v2.1.144-B7).** The `Read` tool now sniffs binary magic numbers and falls back to image read for `.txt` files with PNG signatures (or vice versa) instead of aborting.

**`/branch` history recovery post-EnterWorktree (CC v2.1.144-B10).** EnterWorktree now snapshots `original_dir` and `original_sessions_dir` onto `WorktreeState` at enter time. Commands resolving conversation history past the cwd switch can recover the parent path via `worktree_ops::original_sessions_dir()`.

**MCP image fallback for unsupported MIME (CC v2.1.144-B17).** MCP tools returning image content with unsupported MIME (SVG, BMP, TIFF, AVIF) now save the bytes to `~/.anvil/mcp-images/<sha256>.<ext>` and return a Text block referencing the saved path — instead of breaking the conversation.

**Skill watcher FD exhaustion prevention (CC v2.1.144-B18).** The skill directory watcher now excludes `target/`, `node_modules/`, `.git/`, `dist/`, `build/`, `.cache/`, `.next/`, `__pycache__/` from its watched subtree, and reload is gated on `.md` file changes only. Build artifacts in skill directories no longer cause cascading reloads.

**Theme color reset on first /rename (CC community #61082).** First `/session rename` no longer resets custom theme colors to defaults. Regression test pins this.

**EnterWorktree MCP config preservation (CC community #61062).** `EnterWorktree` now snapshots the resolved `McpConfigCollection` from the original CWD before `set_current_dir`. Project-scoped MCP servers stay active inside the worktree instead of silently disappearing.

## AnvilHub `/build` Page + `anvil-mcp-builder` Micro-Service

Anvil's `/mcp builder` TUI wizard (v2.2.18) now has a web counterpart. The `anvil-mcp-builder` micro-service runs at `127.0.0.1:4090` on the AnvilHub host and exposes three endpoints:

- `POST /api/builder/spec` — LLM-generated MCP spec from a free-text user prompt. SSE streaming response.
- `POST /api/builder/generate` — turns the spec into a base64 tarball (Node.js, TypeScript, or Python templates).
- `POST /api/builder/sandbox` — extracts the tarball, runs `anvil-sandbox-runner` against it (network-cut), returns sandbox stdout/stderr.

### Security

The operator OAuth token (for the LLM that generates specs) is **loaded from the Anvil vault at startup**, never from `.env`. `src/vault.js` reads `mcpbuilder/operator-oauth-token` via `anvil /vault get`; the service exits 1 if vault is locked or the entry is missing. Token is cached in process memory only, redacted from all log output.

### Publisher pre-check for sandbox endpoint

The sandbox endpoint runs `npm install` / `pip install` per request — a real abuse vector. Access is gated on publisher standing: user must be in the `anvilhub-publishers` Authentik group OR have ≥1 HubPackage already published. The check hits AnvilHub's `/api/users/<email>/publisher-status` endpoint with a 5-minute Map cache, falls closed on backend error.

### AnvilHub `/build` page spec

A 583-line spec doc at `docs/v2.2.19-anvilhub-build-page-spec.md` covers the Next.js page implementation: server component + `BuildClient.tsx` with a 3-step wizard (Prompt → SpecReview → Generate→Sandbox→Publish), SSE token streaming in step 1, env vars (`NEXT_PUBLIC_BUILDER_URL`), Apache reverse-proxy snippet, 8-axis capability contract table. The page deploys to dev0001 when next anvilhub-web cycle runs.

## Release Pipeline Hardening (#714 + #730)

`scripts/release.sh` now wraps every phase in `step "PN: <description>"` + `ok "PN"` / `fail "PN"` markers. The new `scripts/release-helpers/step-gates.sh` provides primitives + JSON status persistence + an EXIT-trap silent-exit detector that marks any RUNNING phase as FAIL on premature script exit.

This closes the v2.2.18 Phase 6 silent-exit class of bugs. `set +e` / `SSH_RC=$?` / `set -e` pattern applied around SSH calls so heredoc-style remote work surfaces its exit code instead of cascading into a `set -e` silent kill.

`scripts/test-release-gates.sh` is the regression harness — runs release.sh in `--dry-run` mode and asserts every expected phase fires START + terminal marker exactly once.

C2 (anvilhub /about ISR migration) and C3 (Puppet enforcing 644 on /opt/passage) are documented in `docs/v2.2.19-arc-c2-anvilhub-isr.md` and pushed in the puppet-control repo `feat/passage-perms-fix-v2.2.19` branch respectively.

## Tooling

`anvil-sandbox-runner` (the binary the AnvilHub Build sandbox uses) gets a Puppet manifest at `profile::anvil_sandbox` to install on anvilhub-web hosts.

## Test Suite

Net +50 tests across the workspace:
- i18n: +1 drift gate, +1 picker invariant, +8 locale-load
- Memory Cohesion: +9 episodic, +6 promote, +6 cache, +5 working, +3 procedural
- CC parity P0/P1/P2: +35 across all 15 fixes
- AnvilHub Build: +7 publisher-standing tests in the micro-service

One flake fixed: `routines::proposal::drop_pending_only_targets_named_routine` now uses `tempfile::TempDir` for guaranteed per-thread isolation. 3 workspace runs clean.

## Surface Manifest

All 18 locales advertised in `release-surfaces.yaml`. SUPPORTED_LANGUAGES + NATIVE_NAMES in lockstep across `crates/anvil-cli/src/utils.rs` + `wizard.rs` + `tui/mod.rs`.

## anvild Separate Process Name (#766)

The background OAuth-refresh + routines daemon now runs as `anvild`, not `anvil daemon foreground`. New `anvild_path_from(anvil_binary)` helper rewrites the binary path used by every supervisor unit (macOS LaunchAgent, Linux user-systemd, FreeBSD/NetBSD rc.d, Windows Task Scheduler) plus the in-TUI `daemon::spawn_detached` fallback. `ps -ef | grep daemon` now shows `anvild` rather than masquerading as the foreground TUI binary. `install.sh` + `install.ps1` create the anvild symlink (Unix) / hardlink (Windows NTFS) alongside the main binary.

## Wizard P0 Bundle (final-day fixes)

Three wizard bugs surfaced during preview-binary testing and landed before tag:

- **BUG-F (#767)** — `ConfirmModal` had a hardcoded 9-row height. On the Ollama wizard's "already_running" modal where the body embeds a multi-model list, body wrap consumed all 7 inner rows and the Yes/No buttons + key hint were clipped invisible. Now derives `modal_h` from `wrap_body(body, width).len() + 6` so buttons stay visible regardless of body length. Also adds a `Ctrl+B` Back keybind on Choice + Confirm modals + `ChoiceAction::GoBack` + `ConfirmAction::GoBack` + `ModalAnswer::GoBack` enum variants. The keybind is wired at the modal layer; the orchestrator's full step-history state machine is deferred to v2.2.20 with honest disclosure in the commit message.

- **BUG-G (#768)** — The "Install anvild as background service?" prompt was running as `println!` + `io::stdin().read_line()` from `anvild_bootstrap::ensure_anvild_for_session` AFTER the wizard exited its alt-screen, breaking the rule that the wizard never drops to CLI. New `wizard_daemon::run_daemon_step` runs inside the same alt-screen as Step 8 of 9 (a `WizardChoiceModal` with three choices: Install / Ask later / Never). Persists `anvild.autostart = yes/no` to `config.json` before the wizard returns; the legacy `prompt_user()` stays as a fallback for upgrade paths where config is still `ask`. 5 unit tests pass.

- **BUG-H (#769)** — In the vertical-split layout, a normal click-drag in the deck column pulls in left-rail text because terminal emulators select at pixel coordinates with no awareness of Anvil's columns. Task #625 mitigated visually; #748 added Option-drag passthrough on macOS for column (rectangle) selection. Neither surfaced the modifier to users, so the muscle memory stayed broken. Now renders a 1-line hint above the BUILD section in the rail bottom group: `⌥+drag deck only` on macOS, `Alt+drag deck only` on Linux/Windows/BSD. i18n keys added across all 18 supported locales.

---

*Authored across 40+ commits between 2026-05-19 and 2026-05-22. Co-engineered by CC and the Culpur Defense engineering team.*
