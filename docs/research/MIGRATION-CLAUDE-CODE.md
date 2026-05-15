# Migration — Claude Code → Anvil

**Status:** draft for review, 2026-05-15.
**Companion to:** [`SEVEN-LAYER-MEMORY.md`](SEVEN-LAYER-MEMORY.md), [`CAPABILITY-COHESION.md`](CAPABILITY-COHESION.md).
**Direction:** [`feedback-anvil-migration-instinct.md`](../../../.claude/projects/-Users-soulofall-projects/memory/feedback-anvil-migration-instinct.md).
**Target release:** v2.2.14 (Phase 6 of the v2.2.14 arc; lands before tag).

---

## 1. The problem

A user with months or years of Claude Code state wants to move to Anvil. The CC install has accumulated:

- ~94 memory entries (behavioral rules + project knowledge)
- ~9 CLAUDE.md instruction files across projects
- ~1801 session transcripts (1 GB of conversation history)
- Settings, skills, plugins, agents

This is the user's brain-extension. Losing it is migration friction the AI assistant world has not solved well. The user's instinct, verbatim: **"bring everything we can, full migration."**

Mental model: importing from Gmail, not scanning some markdown.

---

## 2. The shape

ONE command does the migration:

```
anvil import claude-code [--dry-run] [--scope=current-project|global|all] [--include-sessions]
```

Defaults: `--scope=all`, sessions excluded (separate flag because expensive).

The first-run wizard (task #427) detects `~/.claude/` on startup and offers the migration as the natural onboarding moment. Users who skip can run the command later.

---

## 3. The pipeline

Seven stages, each revert-safe:

```
1. Discover    — enumerate ~/.claude/ artifact types
2. Triage      — categorize each into Anvil tier + skip list
3. Translate   — frontmatter stamps; light path/identity rewrites
4. Stage       — write to ~/.anvil/.import-staging/
5. Diff        — show user what will land where
6. Confirm     — user accepts or cancels
7. Commit      — atomic move from staging to final destinations
8. Report      — manifest + skipped-with-reasons + needs-review list
```

Read-only on `~/.claude/`. We never modify CC's state.

---

## 4. Artifact → Anvil tier mapping

The bring/skip matrix. Per the migration instinct: bring as much as possible.

### Bring

| Source | Destination | Notes |
|---|---|---|
| `~/.claude/projects/<id>/memory/*.md` | `~/.anvil/memory/` | Verbatim. Keep YAML frontmatter. Add `imported_from: claude_code`. Per-project memory dirs all merge under one user-level Anvil memory. |
| `~/.claude/CLAUDE.md` | `~/.anvil/ANVIL.md` (global) | If `~/.anvil/ANVIL.md` exists, append under a `## Imported from Claude Code (YYYY-MM-DD)` heading; don't clobber. |
| `<project>/CLAUDE.md` | `<project>/ANVIL.md` | Only if no `ANVIL.md` exists. Otherwise stage as `ANVIL.imported.md` for user to merge. |
| `~/.claude/settings.json` | `~/.anvil/settings.json` | Translate per schema (§5). Conflicting keys staged for user review. |
| `~/.claude/skills/*` (if exists) | `~/.anvil/skills/` | Skills carry compatible front-matter. Plugin-marketplace skills go through plugin loader. |
| `~/.claude/plugins/installed_plugins.json` + `marketplaces/*` | `~/.anvil/plugins/` | Translate manifest where possible. Unknown shape → staged with `needs_review` flag. |
| `~/.claude/agents/*.json` (if exists) | `~/.anvil/agents/` | CC's agent definitions, where format aligns with Anvil's `AgentEntry`. |
| `~/.claude/projects/<id>/*.jsonl` | `~/.anvil/daily/` via LLM summarization | One DailySummary per session per day. NOT raw copy. Gated by `--include-sessions` flag because expensive. |
| `~/.claude/sessions/*.json` (metadata sidecars) | `~/.anvil/sessions/` index | Merge into Anvil's session index for `--resume` recall. |

### Skip (with reason in the import report)

| Source | Reason |
|---|---|
| `~/.claude/.credentials.json` | Server-scoped OAuth identity. User runs `anvil login`. |
| `~/.claude/cache/`, `image-cache/`, `paste-cache/`, `shell-snapshots/` | Transient. No signal. |
| `~/.claude/telemetry/`, `statsig/`, `chrome/` | CC-specific analytics. No Anvil equivalent. |
| `~/.claude/plans/` | Per-session ephemera. Lessons already in memory. |
| `~/.claude/todos/` | Per-session ephemera. |
| `~/.claude/.last-cleanup`, `mcp-needs-auth-cache.json`, `stats-cache.json` | Internal CC state. |
| `~/.claude/history.jsonl` | Global CC interactive history. Pollution risk; not a session record. |

---

## 5. Settings translation

The schemas differ, so this is non-trivial:

| CC settings.json | Anvil settings.json | Translation |
|---|---|---|
| `theme: "dark"` | `theme: "dark"` | Direct map (Anvil supports dark/light/auto; task #347) |
| `outputStyle` | `output_style` | Map values: `concise → condensed`, default → `precise` (Anvil default) |
| `hooks.PreToolUse[]` | `hooks.pre_tool_use[]` | Same shape after Phase 5.3 unified RuntimeHookSpec. CC `command` form → Anvil `Command`. `args[]` form → Anvil `Exec`. |
| `permissions.allow[]`, `permissions.deny[]` | `permissions.allow[]`, `permissions.deny[]` | Direct map. |
| `permissions.defaultMode` | `permissionMode` | Direct map (`acceptEdits`, `plan`, `bypassPermissions`, `default`). |
| `autoMode.hard_deny[]` (CC v2.1.136+) | `autoMode.hard_deny[]` | Direct map (Phase 5.1). |
| `worktree.baseRef` | `worktree.baseRef` | Direct map (Phase 5.1). |
| `effort_level` | `effort_level` | Direct map. |
| `enabledPlugins{}` | `enabledPlugins{}` | Direct map, but plugin names may need translation if Anvil hub uses different IDs. |
| `mcpServers{}` | `mcpServers{}` | Direct map (compatible schema). |
| `claudeAiOauth.*` | (skip — credentials) | DO NOT IMPORT. User re-authenticates. |

Conflicts (key exists in both with different values) are staged for user review in `~/.anvil/.import-review/settings.diff` and never auto-applied.

---

## 6. Session summarization (the expensive bit)

Sessions are 1 GB and the user's most valuable history. Raw copy = pollution. Summarized copy = signal.

### Trigger

`--include-sessions` flag. Defaults to OFF because of LLM cost. The wizard explicitly asks: "Summarize 1801 past Claude Code sessions? (will take ~30 min, ~$5 in tokens)".

### Per-session pipeline

1. Read the JSONL transcript (skip oversized — cap 50 MB / session)
2. Extract the first user message, the model identifier, the duration
3. If size < 50 KB → send the whole thing to a small/fast model (Haiku, gpt-oss:120b, or qwen3-coder:latest depending on configured providers) for a 300-word summary
4. If size 50 KB – 50 MB → chunk by user-turn boundaries, summarize each chunk, then summarize-the-summaries
5. Extract structured fields: `tasks_completed[]`, `tasks_open[]`, `files_modified[]`, `lessons_learned[]`
6. Write one DailySummary per session per day to `~/.anvil/daily/<YYYY-MM-DD>.json` (merging sessions on the same day)
7. Each `lesson_learned` becomes a nomination candidate in `~/.anvil/nominations/` — user later reviews via `/memory show semantic --pending`

### Provider choice

Use the cheapest configured provider that's not the user's main model. Local Ollama if available (free); otherwise an explicit "use which provider" prompt.

### Progress

Runs in background (Anvil's `task` infra). User can `Ctrl+C` to cancel; progress survives via `~/.anvil/.import-staging/sessions-progress.json`. Re-running the command resumes from last completed.

---

## 7. Discovered defects (existing CC artifacts that don't map cleanly)

These need surfacing in the import-review report, not silent translation:

1. **CC skills with bash-script hooks** — Anvil's hook executor allows them (Phase 5.3 `string[]` form), but the security model differs. Each hook gets `imported_from: claude_code` and lands disabled by default; user explicitly enables.
2. **CC settings has `additionalDirectories` for filesystem allowlist** — Anvil's sandbox uses `allowedMounts` (different name, same idea). Translate, but flag.
3. **CC project paths in memory entries** — references to `~/.claude/projects/<id>/` won't resolve in Anvil. Don't rewrite at import; verbatim-with-flag means the model interprets at query time.
4. **CC `MEMORY.md` is the auto-loaded index** — Anvil doesn't have one. The CC `MEMORY.md` IS the index of feedback-files. We import the feedback files individually; the `MEMORY.md` itself becomes a record in `~/.anvil/.import-manifest.json`, not loaded into prompts.
5. **CLAUDE.md vs ANVIL.md ownership** — Anvil's auto-loader walks the project tree and merges every `ANVIL.md` it finds. Importing `<project>/CLAUDE.md` could conflict with a user-written `ANVIL.md`. Always stage; never clobber.

---

## 8. 8-axis capability contract

Per [`feedback-anvil-capability-contract.md`](../../../.claude/projects/-Users-soulofall-projects/memory/feedback-anvil-capability-contract.md):

1. **Definition** — `SlashCommand::Import { source: ImportSource, dry_run: bool, scope: ImportScope, include_sessions: bool }` enum variant.
2. **Registration** — entry in `slash_command_specs()` with subcommand list `[claude-code | file | url]` (future-proof).
3. **Completion** — TAB cycles sources + scope values.
4. **Handler** — `commands/src/handlers.rs::handle_import`, delegating to `crates/runtime/src/import/`.
5. **Dispatch** — routes via `commands/dispatch.rs::dispatch_slash_command`.
6. **Rendering** — staged-diff view via Phase 5.2's `ResultBlock` schema. Confirmation prompt before commit.
7. **Permission gate** — writes to `~/.anvil/` route through the gate; reads from `~/.claude/` are read-only and gate-free.
8. **OTel + tests** — `import.discovered`, `import.translated`, `import.staged`, `import.committed`, `import.skipped{reason}` events. Tests cover: idempotency (re-run = no-op on unchanged source), dry-run mode, conflict staging, session-summarization with mock provider.

---

## 9. Buckets

Mirror SEVEN-LAYER-MEMORY.md / CAPABILITY-COHESION.md structure. Total scope **~6–8 engineer-days** plus session-summarization is its own arc.

### Bucket 0 — Foundation (~0.5 days)

- New crate `crates/runtime/src/import/` with `Discover`, `Triage`, `Translate`, `Stage`, `Commit` modules.
- `ImportArtifact` enum (Memory, Instructions, Settings, Skill, Plugin, Agent, Session).
- `~/.anvil/.import-manifest.json` schema for tracking imported items.
- Idempotency: content hash field on every imported file.

### Bucket 1 — Memory + Instructions (~1 day)

- Memory entry import (the 94 markdown files) → `~/.anvil/memory/`
- CLAUDE.md global + per-project → ANVIL.md (with merge semantics, never clobber)
- `imported_from: claude_code` + `imported_at` + `source_path` + `content_hash` stamps
- Tests: re-run produces no changes on unchanged source

### Bucket 2 — Settings + Skills + Plugins (~1.5 days)

- Settings translation per §5 table
- Skills directory import
- Plugin manifest translation (Phase 5.1 added skills/agents fields)
- Conflict staging for review

### Bucket 3 — Sessions (background arc, ~3-4 days, **sequential after 6.1+6.2+6.4**)

- The expensive bit. Runs OFF by default.
- **Sequencing locked 2026-05-15:** sequential after Buckets 6.1+6.2+6.4 land and validate. Files migration must work end-to-end before sessions arc builds on top of it. Lower-risk than parallel.
- Per-session summarizer via configured provider
- DailySummary writer
- Lesson extraction → nominations
- Resumable progress via `.import-staging/sessions-progress.json`

### Bucket 4 — Wizard + UX (~1 day)

- Integrate into first-run wizard (task #427)
- `anvil import claude-code` slash command
- `--dry-run`, `--scope`, `--include-sessions` flags
- Confirmation TUI before commit
- Final report markdown at `~/.anvil/.import-report.md`

### Bucket 5 — Day-2 cleanup (separate command, requires user authorization to scope-in)

- `anvil memory clean` — LLM rewrite over imported entries
- Strips CC-specific references, normalizes vocabulary
- Optional, user-triggered. NOT silently deferred — if user authorizes inclusion in this arc, it lands; otherwise it's flagged as an open follow-up at v2.2.14 tag time for explicit user decision.

---

## 10. Strict SKIP list (what this arc does NOT do)

- **No automatic execution of imported skills.** Imported skills land disabled. User explicitly enables.
- **No credential migration.** OAuth tokens are scoped to CC. `anvil login` is the path.
- **No bidirectional sync.** One-way migration. User keeps using CC alongside Anvil if they want; re-run import after teaching CC new things.
- **No deletion of CC data.** Read-only on `~/.claude/`. User can keep CC fully functional after migration.
- **No silent rewriting.** Verbatim with flag. Day-2 cleanup is a separate command.
- **No release surfaces during the arc.** v2.2.14 ships when its 8-axis contract is met across ALL phases (including migration), not before.

---

## 11. Verification

For each bucket, acceptance test:

- **Bucket 0:** Re-running `anvil import claude-code --dry-run` on the same source twice produces byte-identical staging output.
- **Bucket 1:** Import 94 memory entries; verify all have `imported_from: claude_code` frontmatter; verify `/memory show semantic` lists them; verify hash field detects no-op on re-run.
- **Bucket 2:** Settings.json round-trip — known CC settings translate to expected Anvil keys; conflicting keys land in `~/.anvil/.import-review/`.
- **Bucket 3:** Mock-provider summarization produces a DailySummary per session; lessons extracted become nominations; cancellation mid-run is recoverable.
- **Bucket 4:** First-run wizard detects `~/.claude/`; offers migration; `--scope=current-project` only imports current-cwd-matching artifacts.

---

## 12. What this doc did NOT do

- Did not write any code.
- Did not pre-decide whether the session-summarization model defaults to Ollama vs Anthropic; the spec says "cheapest configured provider that's not the user's main."
- Did not handle the edge case of multiple `~/.claude/` profiles (some users have several). Treat that as v2.2.16 follow-up if it surfaces.
- Did not pre-design the Day-2 cleanup command (`anvil memory clean`). Deferred per §9 Bucket 5.

---

## 13. See also

- [`SEVEN-LAYER-MEMORY.md`](SEVEN-LAYER-MEMORY.md) — the memory arc this importer feeds
- [`CAPABILITY-COHESION.md`](CAPABILITY-COHESION.md) — the 8-axis contract this arc honors
- [`feedback-anvil-migration-instinct.md`](../../../.claude/projects/-Users-soulofall-projects/memory/feedback-anvil-migration-instinct.md) — user's direction
- [`feedback-anvil-capability-contract.md`](../../../.claude/projects/-Users-soulofall-projects/memory/feedback-anvil-capability-contract.md) — never ship without all 8 axes
- Task #427 — first-run wizard (the integration point)
