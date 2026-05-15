# Anvil v2.2.15 — The Largest Release in Anvil's History

Released: 2026-05-15

**v2.2.15 is the largest Anvil release since the project began.** (v2.2.14 was a corrupted release; v2.2.15 supersedes it.) Two foundational arcs finish what v2.2.5 started, the provider catalog grows from 5 to 31, Cursor lands as a first-class agent surface, and `/model` now picks across every configured provider in one unified list. 121 commits over the v2.2.13 baseline.

## What's new in v2.2.15 (the headline features)

### `/cursor` first-class command tree

Cursor's public Cloud Agents API now drives a dedicated command tree with six TAB-completable subcommands:

- **`/cursor launch <prompt>`** — `POST /v1/agents` against the current workspace's GitHub repo, opens an SSE stream in a new agent tab
- **`/cursor list`** — show all your active agents with status, model, repo, branch
- **`/cursor get <agent_id>`** — full agent record + recent runs
- **`/cursor cancel <agent_id> [<run_id>]`** — terminate an in-flight run
- **`/cursor artifacts <agent_id>`** — list and download generated files
- **`/cursor stream <agent_id> <run_id>`** — re-attach SSE stream with `Last-Event-ID` resume

Cursor is repo-bound by design (`git remote get-url origin` is mandatory) — errors loudly if the current workspace isn't a GitHub repo. No silent failure, no fallback. 8-axis capability contract green on all six subcommands.

### `/model` cross-provider unified picker

`/model` now enumerates **every configured provider's models in one TAB-able list**, provider-prefixed for clarity. Type `/model<TAB>` and see:

```
anthropic/claude-4.5-sonnet
groq/llama-3.3-70b
gemini/gemini-2.0-flash-thinking
bedrock/anthropic.claude-3-5-sonnet-20241022-v2:0
cursor/claude-4-sonnet-thinking
ollama/qwen-coder
... 25+ more
```

Picking `cursor/claude-4-sonnet-thinking` performs an **atomic switch**: provider routing, system prompt identity, and TUI chrome all update together. Per `feedback-model-switch-must-be-atomic.md`. Bare model names (without prefix) work too — if exactly one provider exposes that model, switch directly; if multiple, error with the qualified options.

Unconfigured providers are excluded from the picker. Live-list per provider; existing per-provider caches reused.

## What landed in this cycle (the foundation)

The work in v2.2.14 (which never shipped publicly) is included in v2.2.15. The full feature set:

### Memory Cohesion Arc (Phase 5.x)

The seven-layer memory system from v2.2.5 finally cohesion-tests end-to-end:

- Retrieval-order block added to the system prompt so the model knows which memory layers exist and when to consult them
- `WorkingMemorySnapshot` lands as `Vec<PromptSection>`
- L3 `memory_summary` path mismatch fixed
- L5 vault.bin init-marker fixed
- L6 PermissionMemory wired into the permission gate
- L7 `/file-cache` real handler replaces the stub; budget cache path mismatches fixed
- Egress allowlist wired into `settings.json` + `/policy view`
- `/memory why` now actually injects DailyStore daily summaries into the prompt

### Capability Cohesion Arc (Phase 5.x)

Every Anvil capability now meets an 8-axis contract: definition / registration / completion / handler / dispatch / rendering / permission gate / OTel + tests. The cheap-drift gate enforces this at workspace build time — `every_slash_command_variant_has_a_spec` blocks bidirectional drift between `SlashCommand` and the spec table. Slash dispatch is unified at `commands/dispatch.rs::dispatch_slash_command`. Stub messages are banned.

### CC→Anvil Migration Arc (Phase 6)

`anvil import claude-code` brings your CC investment forward verbatim with provenance frontmatter:

- **Memory entries** (typical CC profile = 94 markdown files) → `~/.anvil/memory/` with `imported_from: claude_code` stamps and content hashes for idempotent re-runs
- **`CLAUDE.md` files** (global + per-project) → `ANVIL.md` with merge semantics — never clobbers existing files, always stages first
- **`settings.json`** with conflict detection — actual conflict counts surface, not booleans
- **Skills** import with collision handling; disabled by default until you explicitly enable
- **Plugin manifests** with the new skills/agents fields
- **Past sessions** (up to ~1,800 JSONL files, ~1 GB) — gated behind `--include-sessions` and summarized via your configured provider into `~/.anvil/daily/` records. Resumable across runs; fails loud if no provider configured.

The day-2 cleanup command `anvil memory clean` rewrites imported entries through a configurable LLM, normalizes vocabulary, and detects duplicate-meaning entries via Jaccard similarity + LLM judge. Provenance frontmatter is preserved verbatim. The first-run setup wizard offers the migration as an opt-in step.

### Provider catalog expansion (5 → 31)

Every provider implementation either uses a documented public API or identifies as Anvil honestly in headers. No IDE spoofing, no scraped credentials, no silent fallbacks.

**New OpenAI-compatible providers** (groq, fireworks, mistral, perplexity, deepseek, togetherai, deepinfra, cerebras, nvidia-nim, huggingface, moonshotai, nebius, scaleway, stackit, baseten, cortecs, 302ai, zai, openrouter, lmstudio, chutes, minimax, opencode, opencode-go) — each gets a dedicated config block and is selectable via `/provider <slug>`.

**New dedicated heavyweight clients:**

- **Cursor** — public Cloud Agents API, repo-bound (see `/cursor` above)
- **GitHub Copilot** — device flow
- **Azure OpenAI** — deployment-name + `api-version` URL pattern
- **AWS Bedrock** — manual SigV4 signing (no AWS SDK dependency, ~50 MB smaller binary), audited against AWS test vectors. `InvokeModel` + `InvokeModelWithResponseStream`
- **Gemini OAuth + Antigravity** — Google Code Assist OAuth (PKCE flow, dynamic-port localhost callback, atomic token save, pre-emptive refresh). Anvil identifies as `User-Agent: Anvil/2.2.15 ({os})` and `x-goog-api-client: anvil-cli` — no VS Code spoofing

`/provider login <slug>` runs the right flow for each: API-key paste + validation for direct-key providers, OAuth PKCE for Google, device flow for Copilot, SigV4 for Bedrock. Atomic provider/model switch updates routing + system prompt identity + TUI chrome together.

### CC parity catch-up (v2.1.132 → v2.1.139)

17 features + 7 security/verify items filed and closed: `ANVIL_PROJECT_DIR` in MCP child env, `ANVIL_SESSION_ID` in Bash subprocess + hooks, `ANVIL_DISABLE_ALTERNATE_SCREEN`, `ANVIL_EFFORT` env + hook field, transcript view nav, cross-session agent view, `worktree.baseRef`, `autoMode.hard_deny`, hook `continueOnBlock` for PostToolUse, hook args `string[]` exec form, `/scroll-speed`, `/goal` name collision, `anvil plugin details`, `--include-sessions`, subagent OTel headers with parent-agent-id, plus the security gates on `--resume/--continue` underscore paths, plan-mode + Edit allow rule write blocking, MCP content-block result visibility, Skill(name *) wildcard prefix matching, settings hot-reload for symlinked files, stream-idle false-fire after stream complete, multi-image paste.

### Token economy + reliability

- File-fingerprint cache wired (the module shipped in v2.2.13 without callsites — now actually saves tokens on re-read)
- Command-output cache wired into `glob_search` and `grep_search`
- WebFetch + WebSearch get 5-min and 1-hour TTL caches
- Skill-chaining engine depth-3 + wired (suggestion engine; full executor lands in v2.2.16)
- Auto-promote engine: `/memory` commands now reach the engine
- Release notes embedded via `include_str!` across README, AnvilHub /about, culpur.net/anvil

### Honesty contract additions

The release process itself got tighter. Several feedback rules were codified as test-suite-enforced contracts during this cycle:

- **No silent deferral** — completion claims that hide pending work get caught at the build gate
- **8-axis capability contract** — every command ships all 8 axes (definition / registration / completion / handler / dispatch / rendering / gate / OTel+tests) or it does not ship
- **CC-only naming** — Anvil code/changelog/docs say "CC", never "Claude Code"
- **Changelog preservation** — historical changelog entries are byte-immutable on every release
- **Migration instinct** — when users migrate from CC, bring everything we can, verbatim-with-flag
- **Live-model-list, not registry** — `/model` reads live provider APIs; static catalog is fallback only
- **Atomic provider/model switch** — routing + system prompt identity + TUI chrome update together, never separately

## Platforms

Seven targets, same as v2.2.13: macOS arm64 + x86_64, Linux arm64 + x86_64, Windows x86_64, FreeBSD x86_64, NetBSD x86_64. SHA256s published to anvilhub.culpur.net/sha256.

## Upgrading

```
brew upgrade anvil   # macOS / Linux
```

Or download the binary for your platform from the GitHub Release page. `anvil --version` will report `2.2.15`. First run after upgrade may show the migration wizard if your CC profile is detected — opt in or skip; you can run `anvil import claude-code --dry-run` later to preview.

If you're upgrading from a pre-v2.2.13 version: read v2.2.13's notes for the BSD cross-compile additions and `/ssh` tab.
