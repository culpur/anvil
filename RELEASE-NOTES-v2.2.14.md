# Anvil v2.2.14 — Memory Cohesion, Migration Arc, and 31 Providers

Released: 2026-05-15

v2.2.14 is a foundational release. Two large arcs land together: the **Memory Cohesion + Capability Cohesion phases** (5.x) that make memory and slash commands behave the way the system prompt always promised they would, and the **CC→Anvil migration arc** (Phase 6) that brings your CC investment forward verbatim. On top of that, the provider catalog expands from 5 to **31 providers** with honest identification across the board — no IDE spoofing, no stubs, no silent fallbacks.

## CC→Anvil migration arc (Phase 6)

`anvil import claude-code` now brings everything forward verbatim with provenance frontmatter:

- **Memory entries** (94 markdown files in the typical CC profile) → `~/.anvil/memory/` with `imported_from: claude_code` stamps and content hashes for idempotent re-runs
- **`CLAUDE.md` files** (global + per-project) → `ANVIL.md` with merge semantics — never clobbers existing files, always stages first
- **`settings.json`** with conflict detection — actual conflict counts surface, not booleans
- **Skills** import with collision handling; disabled by default until you explicitly enable
- **Plugin manifests** with the new skills/agents fields
- **Past sessions** (up to ~1,800 JSONL files, ~1 GB) — gated behind `--include-sessions` and summarized via your configured provider into `~/.anvil/daily/` records. Resumable across runs; fails loud if no provider configured.

The day-2 cleanup command `anvil memory clean` rewrites imported entries through a configurable LLM, normalizes vocabulary, and detects duplicate-meaning entries via Jaccard similarity + LLM judge. Provenance frontmatter is preserved verbatim; the rewrite adds new fields alongside, never replaces.

The first-run setup wizard now offers the migration as an opt-in step. The full design and 8-axis capability contract live in `docs/research/MIGRATION-CLAUDE-CODE.md`.

## Provider catalog expansion (5 → 31)

Every provider implementation either uses a documented public API or identifies as Anvil honestly in headers. No IDE impersonation, no scraped credentials.

**New OpenAI-compatible providers** (groq, fireworks, mistral, perplexity, deepseek, togetherai, deepinfra, cerebras, nvidia-nim, huggingface, moonshotai, nebius, scaleway, stackit, baseten, cortecs, 302ai, zai, openrouter, lmstudio, chutes, minimax, opencode, opencode-go) — each gets a dedicated config block and is selectable via `/provider <slug>`. Unconfigured providers are hidden from the `/provider list` picker until their auth env or saved credential is present.

**New dedicated heavyweight clients:**

- **Cursor** — public Cloud Agents API. `GET /v1/models`, `POST /v1/agents`, full SSE event parsing (`assistant`, `thinking`, `tool_call`, `status`, `result`, `done`, `error`). Requires a GitHub repo binding (auto-detected from `git remote get-url origin`); errors loudly if the current workspace is not a GitHub repo rather than silently failing.
- **GitHub Copilot** — device flow.
- **Azure OpenAI** — deployment-name + `api-version` URL pattern.
- **AWS Bedrock** — manual SigV4 signing (no AWS SDK dependency, ~50 MB smaller binary), audited against AWS test vectors. `InvokeModel` + `InvokeModelWithResponseStream`.
- **Gemini OAuth + Antigravity** — Google Code Assist OAuth (PKCE flow, dynamic-port localhost callback, atomic token save, pre-emptive refresh). Anvil identifies as `User-Agent: Anvil/2.2.14 ({os})` and `x-goog-api-client: anvil-cli` — no VS Code spoofing.

`/provider login <slug>` runs the right flow for each: API-key paste + validation for direct-key providers, OAuth PKCE for Google, device flow for Copilot, SigV4 for Bedrock. Atomic provider/model switch updates routing + system prompt identity + TUI chrome together.

## Memory Cohesion Arc (Phase 5.x)

The seven-layer memory system from v2.2.5 finally cohesion-tests end-to-end:

- Retrieval-order block added to the system prompt so the model knows which memory layers exist and when to consult them
- `WorkingMemorySnapshot` lands as `Vec<PromptSection>` (no more monolithic prompt slop)
- L3 `memory_summary` path mismatch fixed
- L5 vault.bin init-marker fixed
- L6 PermissionMemory wired into the permission gate
- L7 `/file-cache` real handler replaces the stub; budget cache path mismatches fixed
- Egress allowlist wired into `settings.json` + `/policy view`
- `/memory why` now actually injects DailyStore daily summaries into the prompt

## Capability Cohesion Arc (Phase 5.x)

Every Anvil capability now meets an 8-axis contract: definition / registration / completion / handler / dispatch / rendering / permission gate / OTel + tests. The cheap-drift gate enforces this at workspace build time — `every_slash_command_variant_has_a_spec` blocks bidirectional drift between `SlashCommand` and the spec table.

Slash dispatch is unified at `commands/dispatch.rs::dispatch_slash_command`. Stub messages are banned. Stream B's `mcp_tool` hook entries are wired through `settings.json`. TeamDelegate delegations surface in `agents-live` with `parent_agent_id`. CC parity bugs BUG-3/4 (subagent permission_mode inheritance) fixed.

## CC parity catch-up (v2.1.132 → v2.1.139)

Filed and closed: 17 features (`ANVIL_PROJECT_DIR` in MCP child env, `ANVIL_SESSION_ID` in Bash subprocess + hooks, `ANVIL_DISABLE_ALTERNATE_SCREEN`, `ANVIL_EFFORT` env + hook field, transcript view nav, cross-session agent view, worktree.baseRef, autoMode.hard_deny, hook continueOnBlock for PostToolUse, hook args string[] exec form, /scroll-speed, /goal name collision, `anvil plugin details`, `--include-sessions`, subagent OTel headers with parent-agent-id) and 7 security/verify items (--resume/--continue underscore paths, plan-mode + Edit allow rule write blocking, MCP content-block result visibility, Skill(name *) wildcard prefix matching, settings hot-reload for symlinked files, stream-idle false-fire after stream complete, multi-image paste).

## Token economy + reliability

- File-fingerprint cache wired (the module that v2.2.13 shipped without callsites — now actually saves tokens on re-read)
- Command-output cache wired into `glob_search` and `grep_search`
- WebFetch + WebSearch get 5-min and 1-hour TTL caches
- Skill-chaining engine depth-3 + wired
- Auto-promote engine: `/memory` commands now reach the engine (not just stubs)
- Release notes embedded via `include_str!` across all three surfaces (README, AnvilHub /about, culpur.net/anvil)

## Honesty contract additions

The release process itself got tighter. Several feedback memories were added during this cycle and codified in the test suite:

- `feedback-no-silent-deferral.md` — no "deferred to vN+1" sneak-pasted into a complete claim. The settings_conflict_count refactor and `--include-sessions` fail-loud changes are the canonical fixes.
- `feedback-anvil-capability-contract.md` — every capability ships all 8 axes or it does not ship.
- `feedback-cc-only-naming.md` — Anvil code/changelog/docs say "CC", never "Claude Code". Even on parity work.
- `feedback-changelog-preserve-historical.md` — historical changelog entries are byte-immutable on every release.
- `feedback-anvil-migration-instinct.md` — when users migrate from CC, bring everything we can, verbatim-with-flag.
- `feedback-model-list-is-live-not-registry.md` — `/model` reads live provider APIs, static catalog is fallback only.
- `feedback-model-switch-must-be-atomic.md` — `/model` switches routing + system prompt identity + TUI chrome atomically.

## Platforms

Seven targets, same as v2.2.13: macOS arm64 + x86_64, Linux arm64 + x86_64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. SHA256s published to anvilhub.culpur.net/sha256.

## Upgrading

```
brew upgrade anvil   # macOS / Linux
```

Or download the binary for your platform from the GitHub Release page. `anvil --version` will report `2.2.14`. First run after upgrade may show the migration wizard if your CC profile is detected — opt in or skip; you can run `anvil import claude-code --dry-run` later to preview.

If you're upgrading from a pre-v2.2.13 version: read v2.2.13's notes for the BSD cross-compile additions and `/ssh` tab.
