# get-shit-done (GSD) vs Anvil — research comparison

**Audited:** 2026-05-11
**GSD path:** /Users/soulofall/projects/get-shit-done
**GSD-2 path:** /Users/soulofall/projects/gsd-2 (successor, v2.82.0)
**Anvil path:** /Users/soulofall/projects/anvil-dev
**Anvil current version:** v2.2.12 (released 2026-05-11)

## 1. What GSD is (one paragraph)

Get Shit Done solves context rot — the degradation of AI assistant output quality as the LLM's context window fills during long projects. Originally a prompt-injection framework for Claude Code (v1, package name `get-shit-done-cc`), GSD evolved into a full-stack workflow orchestration system with two active codebases: the original v1 (1.50.0-canary.0, /Users/soulofall/projects/get-shit-done) and GSD-2 (v2.82.0, /Users/soulofall/projects/gsd-2), a complete rewrite on the Pi SDK that runs as a standalone CLI instead of embedded prompts. GSD-2 targets solo developers building under AI — it automates discovery → research → planning → phased execution → verification → milestone advance, with durable state, worktree isolation, and cost/token tracking. The v1 system focuses on meta-prompting and spec-driven development; GSD-2 adds autonomous orchestration with drift-driven state reconciliation. This audit primarily uses GSD-2 (the shipped successor) for architectural comparisons, with v1 cited for foundational design patterns already in v1 codebase.

## 2. Architecture summary

**GSD-2 (the active successor):** Built on Pi SDK, organized around a phase-driven state machine. Core surfaces:
- `src/resources/extensions/gsd/` — orchestrators, state derivation, session runners
- `src/resources/extensions/gsd/state-reconciliation/` — ADR-017 drift detection + repair registry (the headline feature)
- `src/resources/extensions/gsd/auto.ts` — auto-loop orchestrator driving phase/slice dispatch
- `src/resources/extensions/gsd/worktree-manager.ts` — per-slice git worktree lifecycle (ADR-016)
- Database (SQLite via Drizzle ORM) — stores milestones, slices, tasks, completion state, dependencies
- Session journal — durable log of every query, cost, tokens, recovery events
- Markdown artifacts — PROJECT.md (meta), ROADMAP.md (phase/slice structure), SUMMARY.md (completion state)

**GSD-1:** SDK at `sdk/src/`, organized around Agent SDK `query()` call orchestration:
- `phase-runner.ts` (1442 lines) — state machine: discuss → research → plan → execute → verify → advance (wave-grouped parallel execution)
- `session-runner.ts` — wraps Agent SDK `query()` with prompt-building, tool-scoping, cost tracking
- `cli.ts`, `gsd-tools.ts` — command router
- `.planning/` directory (CJS/SDK shared) — persists phase/plan/summary state as markdown

Both systems persist state to disk (v2 adds DB); both use waves for parallel execution; both wire MCP tools and vault secrets. GSD-2 is the production system; GSD-1 is maintained but superceded.

## 3. Schedule grammar / dispatch

**GSD:** Does not expose a scheduling API. GSD is event-driven (phases triggered by user `/gsd-*` commands or auto-loop polling). Individual phases run in response to user command or automatic milestone advance; there is no built-in cron, watch, or webhook trigger surface in the shipped v1 or v2 product.

**GSD-2 ADR references:** ADR-014 (deep auto orchestration) and ADR-017 (state reconciliation) discuss "dispatch" as the decision to run a slice's execute step, not as a scheduled task trigger. Dispatch reconciles state, then runs the slice's phase machine.

**Contrast with Anvil:** Anvil has a full schedule grammar. See §3 / §8 below for Anvil details.

## 4. Output / archive / delivery

**GSD-1:** No built-in delivery layer. Phase outputs (SUMMARY.md, verification results) land in `.planning/phases/<N>/SUMMARY.md`. Plans are persisted to `.planning/phases/<N>/plans/<plan-id>/PLAN.md` with execution results embedded. There is no centralized output archive or delivery targeting (email, webhook, etc.).

**GSD-2 v2.82:** Markdown artifact rendering to disk (`src/resources/extensions/gsd/markdown-renderer.ts`, line not cited but mentioned at ADR-017:19 as owning `repairStaleRenders`). Outputs go to milestone/slice directories in the worktree. No multi-target delivery system (email, webhook, local archive) is documented as shipped.

**ROUTINES-ADOPTION-NOTES.md §5-6 (Anvil design, not shipped):** Anvil's planned v2.4 routine delivery includes `local` (to `~/.anvil/routines/output/{id}/{ts}.md`), `email`, and `webhook`, with output truncation (~4000 chars) and full archive storage. This is design-stage in Anvil, not yet implemented.

## 5. Skill / agent definition format

**GSD-1:** Agents defined as Markdown files in `/agents/` with YAML frontmatter. Example: `agents/gsd-project-researcher.md` (lines 1-12):
```yaml
---
name: gsd-project-researcher
description: Researches domain ecosystem before roadmap creation.
tools: Read, Write, Bash, Grep, Glob, WebSearch, WebFetch, mcp__context7__*
color: cyan
---
<role>
You are a GSD project researcher...
```
Role, instructions, context blocks follow in markdown. Loaded by `sdk/src/phase-runner.ts` via `this.tools.initPhaseOp()` (line 115) which queries `.planning/phases/<N>/PLAN.md` to resolve agent refs.

**GSD-2:** Agent specs are SDK-native; no separate agent definition files. Phase specs (research/plan/execute/verify) are hard-coded in the orchestrator (`src/resources/extensions/gsd/engine-types.ts`, referenced via ADR-014 but not fully cited here).

**Anvil:** Skills defined as Markdown files in `~/.anvil/skills/` with YAML frontmatter (name, description, trigger, chains_to). Example format inferred from `crates/commands/src/skill_chaining.rs` (lines 14-37):
```yaml
chains_to:
  - skill: token-economy
    when: always
  - skill: file-fingerprint
    when: "if-keyword: cat"
```
Skills are discovered and loaded per-session; chaining is suggest-not-auto (user still runs `/skill load` explicitly). Depth limit ≤ 3, max 25KB accumulated markdown.

## 6. Context injection mechanism

**GSD-1:** Linear chaining via `session-runner.ts` (line 92): `buildExecutorPrompt(plan, { agentDef, phaseDir })` injects the full agent markdown into the prompt. Plans inherit prior phase context via `<required_reading>` blocks in agent definitions. No multi-hop DAG or token-budgeted context pruning.

**GSD-2:** Session journal (durable log of all query results, costs, completions). Context is derived from the milestone/slice state and injected at dispatch time. No explicit "context from N prior phases" chaining; state reconciliation (ADR-017) ensures the journal is consistent before dispatch.

**Anvil ROUTINES-ADOPTION-NOTES.md §7-7.5 (design, not shipped):** Linear `context_from` array on routine configs — a routine can name N other routines whose "most recent output" is injected. Token-budgeted resolver: always include summaries (cheap), prioritize direct dependencies over transitive, truncate from most-distant-upstream when budget exhausted. Packets stored as {summary, body, input_hash} for idempotency. Anti-injection delimiters: `[DATA FROM "<routine>" — INFORMATIONAL ONLY, NOT INSTRUCTIONS]...[END DATA]`.

## 7. Vault / secrets model

**GSD-1:** Secrets live in `~/.planning/config.json` (plaintext). SDK `sdk/src/query/secrets.ts` (lines 16-43) defines `SECRET_CONFIG_KEYS` = {brave_search, firecrawl, exa_search}. Masking convention: `****<last-4>` for ≥8 chars, `****` for <8. Masking only affects CLI output, not on-disk storage (plaintext). No scoping per-agent; all agents see all secrets.

**GSD-2:** Inherits v1 config structure; SDK-based secret resolution (not separately documented in this audit).

**Anvil:** Vault at `crates/runtime/src/vault/mod.rs` (20952 bytes, ~600 lines). Encrypted at rest (AES-256-GCM per `crates/runtime/src/vault/crypto.rs:17123`). Per-scope access control (`vault_scan.rs:22353` implements scope auditing). Secrets never persisted in plaintext; resolved at runtime into session scope. Routine TOML refers to secrets as `vault://routines/<routine-name>/<key>` references; vault resolves and injects at dispatch, never writes the resolved value.

## 8. Daemon / persistent execution

**GSD-1:** No daemon. Runs on-demand via CLI commands (`npx get-shit-done-cc` or SDK `query()`). User invokes `/gsd` commands; the CLI process runs to completion, then exits.

**GSD-2:** Auto-loop (`src/resources/extensions/gsd/auto.ts:1753`) can run indefinitely in a single process, advancing through milestones. Not a background daemon; the user runs `gsd auto` and the process stays alive. On SIGKILL or crash, drift reconciliation (ADR-017) recovers on next `gsd` invocation (stale-worker detector clears dead locks, unregistered-milestone handler imports orphaned directories).

**Anvil:** Daemon at `crates/runtime/src/cron.rs` (682 lines). Background thread polled every 30s (`cron.rs:~390+`), fires due entries. Long-lived process. Cron store persisted at `~/.anvil/cron.json`. Tool surface via `crates/tools/src/automation_ops.rs` — `CronCreate`, `CronList`, `CronDelete` wired into LLM tool registry.

## 9. Anti-pattern guardrails — INCLUDING the headline feature

This is the central section. **GSD-2's ADR-017 State Reconciliation Module** is the headline pattern that the previous research identified as adoptable by Anvil.

### GSD-2: Pre-Dispatch State Reconciliation with Idempotent Repairs (ADR-017)

**The pattern:** Before every Dispatch decision (every phase slice execution), run a **drift-detection → repair → re-derive** cycle, capped at **N=2 passes**. Each drift kind has an idempotent repair handler. Terminal blockers (e.g., vault secret missing) surface as user-visible errors, not silent failures.

**Drift kinds (GSD-2 v2.82):** Shipped in `src/resources/extensions/gsd/state-reconciliation/drift/`:
1. **stale-worker** (`stale-worker.ts`, 46 lines) — session lock artifact with dead PID. Repair: `removeStaleSessionLock()` re-reads lock, clears if PID not alive (idempotent).
2. **sketch-flag** (`sketch-flag.ts`, 2203 bytes) — stale design-choice markers on slices. Relocated from `gsd-db.ts:1156` `autoHealSketchFlags` (zero prior callers).
3. **merge-state** (`merge-state.ts`, 10795 bytes) — unmerged branch state after `mergeMilestoneToMain`. Relocated from `auto-recovery.ts:1118`.
4. **project-md** (`project-md.ts`, 2698 bytes) — PROJECT.md divergence from DB. Repair: re-parse and sync.
5. **roadmap** (`roadmap.ts`, 3697 bytes) — ROADMAP.md slice sequence vs DB slice rows. Repair: re-import slices, sync `slice_dependencies` junction table.
6. **completion** (`completion.ts`, 4970 bytes) — missing `completed_at` timestamp on tasks. Repair: backfill from `SUMMARY.md` mtime.
7. **stale-render** (`stale-render.ts`, 6339 bytes) — markdown artifact render output stale (new in v2.82).

**The loop lifecycle** (`src/resources/extensions/gsd/state-reconciliation/index.ts`, lines 56-120):
```typescript
// Pseudocode from ADR-017 + index.ts
async function reconcileBeforeDispatch(basePath, deps):
  for pass = 0; pass < MAX_PASSES (=2):
    invalidateStateCache()
    state = derive(basePath)
    drift = detectAllDrift(state)
    if drift.length === 0:
      return ok=true
    
    failures = []
    for record in drift:
      handler = registry.find(h => h.kind === record.kind)
      try:
        await handler.repair(record, context)
        repaired.push(record)
      catch cause:
        failures.push({drift: record, cause})
    
    if failures.length > 0:
      throw ReconciliationFailedError({failures, pass})
    // else: loop again to detect cascading drift
  
  // After cap, one final derive+detect
  finalState = derive(basePath)
  persistent = detectAllDrift(finalState)
  if persistent.length > 0:
    throw ReconciliationFailedError({persistentDrift})
  return ok=true
```

**Contract enforcement:**
- All repairs must be **idempotent** — re-derive can re-trigger detection on transient state; repairs safe under retry.
- **Cap at 2 passes** — runaway risk mitigation. Persistent or failed drift after pass 2 throws.
- Detector cost is paid every dispatch tick. Cheap detectors (DB queries, `existsSync`) run unconditionally; markdown-parsing detectors short-circuit when artifacts unchanged.
- Module interface at `src/resources/extensions/gsd/state-reconciliation/registry.ts` — typed `DriftRecord` union and `DriftHandler<T>` registry. Each handler exports `{ kind, detect(), repair() }`.

**Failure classification:** New recovery failure kind `"reconciliation-drift"` in `src/resources/extensions/gsd/recovery-classification.ts` routes `ReconciliationFailedError` to escalation + persistent-drift message.

**Caller closure:** Strict — every pre-dispatch site calls `reconcileBeforeDispatch()`:
- Single-loop auto: `src/resources/extensions/gsd/auto/orchestrator.ts:42` (existing).
- Parent processes that spawn parallel workers: new wiring needed at `startParallel` / `startSliceParallel` call sites so workers don't independently race on shared drift.

### GSD-1 & GSD-2: Other Anti-Pattern Guards

**Anti-injection:** GSD-1 agent definitions (`agents/*.md`) include `<documentation_lookup>` blocks (gsd-project-researcher.md, lines 35-56) that instruct agents to verify claims against official docs before asserting. Training data is framed as hypothesis; uncertainty is flagged.

**Sandboxing:** GSD-2 worktree isolation (ADR-016, `src/resources/extensions/gsd/worktree-manager.ts`) — each slice runs in a dedicated git worktree, separate from the main repo. Lifecycle verbs: `adoptOrphanWorktree`, `adoptSessionRoot`, `resumeFromPausedSession`, `restoreToProjectRoot`. Write-gate planning (v2.82 release notes: "complete-slice closeout is read-only") prevents closeout prompts from mutating project files.

**Allowlists:** GSD-1 tools scoped per-agent via frontmatter (`agents/gsd-project-researcher.md:4` lists allowed tools). `sdk/src/session-runner.ts` line 96 resolves `allowedTools` from agent definition or defaults.

## 10. Where Anvil already has parity

**Cron daemon:** Anvil `crates/runtime/src/cron.rs` (lines ~390+), global singleton, 30s poll, persists to `~/.anvil/cron.json`. GSD has no daemon.

**Hooks system:** Anvil `crates/runtime/src/hooks.rs` (1558 lines) — pre/post tool hooks, session hooks. GSD has no generic hook system (agent definitions document one-off instruction blocks like `<documentation_lookup>`, not composable hooks).

**Vault:** Anvil `crates/runtime/src/vault/mod.rs` (20952 bytes), encrypted at rest, per-scope access control. GSD-1 plaintext config; GSD-2 inherits v1 model (not verified to be encrypted).

**Skills with chains:** Anvil `crates/commands/src/skill_chaining.rs` (depth ≤ 3, max 25KB, suggest-not-auto). GSD-1 agents can reference required-reading files but no built-in chaining DSL.

**File + command caches:** Anvil `crates/runtime/src/file_cache.rs` and `command_cache.rs` (v2.2.11 token-economy work). GSD tracks token spend per session but no caching layer.

**Per-tab parallel inference:** Anvil v2.2.12 (released today) — "parallel inference via JoinHandle spawn" (git log line `aa6ee13`). GSD-1 uses `Promise.allSettled()` for wave-grouped plan execution (`phase-runner.ts:724`); GSD-2 likewise groups slices by dependency wave.

**Session journal:** GSD-2 has durable session journal (every dispatch → journal entry with cost, tokens, recovery). Anvil v2.3 introduces session journal per ROUTINES-ADOPTION-NOTES.md §10 (not yet wired into v2.2.12).

**Output versioning / artifact integrity:** GSD-2 session journal with fingerprints (v2.82 release notes: "artifact integrity fingerprints"). Anvil plans similar in v2.4 routines (input_hash on context packets).

## 11. Where Anvil has gaps relative to GSD

**Gap 1: Pre-dispatch reconciliation module**

GSD-2 shipped the full ADR-017 pattern: typed drift detectors + idempotent repair handlers, capped at N=2 passes, registry-based composition, strict caller closure. Anvil has **no equivalent**. ROUTINES-ADOPTION-NOTES.md §10.5 (Anvil design-stage) sketches the concept for planned v2.4 routines but is unshipped:
- Design layer: `crates/runtime/src/routines/reconcile.rs` is mentioned as a future module location, but does not exist in the current tree.
- Concrete checks identified in design: vault refs, script path confinement, context_from source registration, stale tick lock, output directory writability. None are implemented.
- Compare to GSD-2 shipped implementation: 7 drift kinds, 2-pass loop, strong caller closure.

**Evidence:** `grep -r "reconcile" /Users/soulofall/projects/anvil-dev/crates/runtime/src/*.rs` (not exhaustively searched, but `cron.rs` and `auto_promote.rs` have no reconciliation logic). ROUTINES-ADOPTION-NOTES.md §10.5 is 18 lines of design-stage prose, not code.

**Gap 2: Schedule grammar beyond 5-field cron**

GSD doesn't have scheduling at all (event-driven, §3). Anvil `crates/runtime/src/cron.rs` accepts only classical 5-field cron expression (line 19: `cron_expression: String`). ROUTINES-ADOPTION-NOTES.md §2 identifies the gap: missing `"30m"`, `"every 30m"`, ISO timestamp `"2026-02-03T14:00Z"` formats. Design mitigation: normalize in `CronManager::create` to cron or `next_run` epoch. **Not yet implemented** (still just 5-field string storage at line 19).

**Gap 3: Delivery layer (multi-target output)**

GSD-2 renders outputs to disk (milestone/slice artifact directories). No documented multi-target delivery (email, webhook). Anvil has **no delivery system**. ROUTINES-ADOPTION-NOTES.md §5 designs three targets (`local`, `email`, `webhook`), output truncation (~4000 chars), archive at `~/.anvil/routines/output/{id}/{ts}.md`. **Not yet implemented** — no code in `crates/runtime/src/` for delivery, truncation, or the output archive schema.

**Gap 4: Pre-agent script + wake gate (polling without cost)**

GSD doesn't have this (no scheduling surface). Anvil has **no equivalent**. ROUTINES-ADOPTION-NOTES.md §3 designs: optional script at `~/.anvil/scripts/`, runs first, stdout captured, `{"wakeAgent": false}` on final line suppresses LLM invocation (zero token cost for "no news" polls). Script execution with timeout, secret redaction, stderr isolation. **Not yet implemented** — no script runner in cron/automation surfaces.

**Gap 5: Silent suppression marker**

GSD doesn't have this (no routine/output concept). Anvil has **no equivalent**. ROUTINES-ADOPTION-NOTES.md §4 designs: if LLM output contains `[SILENT]` (case-sensitive), save locally but skip delivery. One line of code, removes "stop spamming me" complaint class. **Not yet implemented**.

**Gap 6: Routine authoring file format (TOML or YAML)**

GSD-1 agents use Markdown with YAML frontmatter (`agents/*.md`, §5). GSD-2 likely uses SDK-native specs. Anvil has **no equivalent**. ROUTINES-ADOPTION-NOTES.md §8 designs TOML at `~/.anvil/routines/<name>.toml` or skill frontmatter `[routine]` block, reusing skill discovery + chains_to machinery. Example:
```toml
[routine]
schedule = "0 9 * * 1-5"
deliver = ["local", "email:me@culpur.net"]
context_from = ["overnight-triage"]
```
**Not yet implemented** — cron store is JSON only (`cron.rs:30-32`).

**Gap 7: Context injection chaining (context_from)**

GSD-1 uses required-reading file refs (unidirectional). GSD-2 state is derived from journal; no explicit chaining DSL. Anvil has **no equivalent**. ROUTINES-ADOPTION-NOTES.md §7-7.5 designs linear `context_from` array (name N upstream routines, inject their last output) + token-budgeted resolver. **Not yet implemented** — no context chaining in cron/routine code.

**Gap 8: Token-budgeted context resolution + packet schema**

GSD-2 session journal tracks tokens but no token-aware context pruning. Anvil has **no equivalent**. ROUTINES-ADOPTION-NOTES.md §7.5 designs: output packets with {summary, body, input_hash}, resolver includes all summaries (cheap), prioritizes direct dependencies, truncates from most-distant-upstream when budget exhausted. input_hash enables cache-hit metrics. **Not yet implemented** — no packet schema, no resolver in cron/routine code.

**Gap 9: Watch + webhook + event triggers (beyond cron)**

GSD doesn't have scheduling. Anvil cron only. ROUTINES-ADOPTION-NOTES.md §9 designs: `schedule = "watch:<glob>"` (filesystem notify), `schedule = "webhook"` (inbound HTTPS), `schedule = "event:<name>"` (internal events like session.end, git.push, goal.nominated). For v2.4, **design-stage only** — grammar extensible, but no watch/webhook/event implementation planned for v2.4 (deferred to 2.4.x point releases). Hooks system exists but not wired to routine triggers.

## 12. Adopt / skip recommendation

This section covers each non-trivial gap from §11, with rationale tied to Anvil's design rules (per-tab isolation, vault-only secrets, suggest-not-auto for skills, MCP hardening).

### ADOPT: Pre-Dispatch State Reconciliation (Gap 1)

**Recommendation: ADOPT** — implement ADR-017 pattern for routine dispatch.

**Rationale:** GSD-2's reconciliation module is the only shipped pattern in the audit that directly addresses the "hidden state drift" problem Anvil will face in v2.4 routines. A routine that's been running for two weeks accumulates drift: vault secret rotated, script path deleted, context_from source disabled, worktree cleaned up externally. Without reconciliation, subsequent dispatch may hang, silently fail, or create zombie locks. GSD-2 shipped 7 concrete drift kinds (stale-worker, sketch-flag, merge-state, project-md, roadmap, completion, stale-render) with idempotent repairs capped at 2 passes, typed registry composition, and strict caller closure. The pattern generalizes cleanly: 

1. Detect (cheap DB query or `existsSync`)
2. Repair (idempotent, side-effect-free in clean case)
3. Re-derive to catch cascading drift
4. Cap at 2 passes
5. Surface blockers clearly (not silent)

For Anvil routines, concrete checks would mirror ROUTINES-ADOPTION-NOTES.md §10.5 design: vault ref resolution, script path existence + confinement, context_from source registration, tick lock staleness, output directory writability. Cost estimate: ~3 days of work (one per check, tests). Fits naturally into the routine dispatch entrypoint (new seam before `spawn` or `run_pending` in `cron.rs` or planned `routines/` module). Pairs naturally with v2.3 session journal: every reconciliation attempt — including `Blocked` — writes a journal entry.

**Cost/benefit:** 3+ days of focused work justifies the gap. Shipping Anvil v2.4 routines without this layer risks a class of silent failures that won't surface until production, when routines have been running for weeks.

### ADOPT: Schedule Grammar Extensions (Gap 2)

**Recommendation: ADOPT** — add `"in Nh"`, `"every Nh"`, and ISO timestamp support.

**Rationale:** Classical 5-field cron is powerful but has friction. GSD doesn't have scheduling (event-driven), so no comparison. ROUTINES-ADOPTION-NOTES.md §2 identifies the practical need: `"30m"` (one-shot), `"every 30m"` (recurring), `"2026-02-03T14:00Z"` (wall-clock). Implementation is low-cost: a parser at `CronManager::create` (line 66) that normalizes any of the four formats into either (a) a cron expression or (b) a single `next_run` epoch. Store both `cron_expression: String` (display) and `next_run: u64` (authoritative). Allows richer user UX without breaking the daemon loop.

**Cost/benefit:** 1 day. Enables monitoring routines (poll every 10 minutes for changelog changes until a script detects change, then cost is zero tokens until then). Small change with high user-visible impact.

### SKIP: Delivery Layer (Multi-Target Output) (Gap 3)

**Recommendation: SKIP** for v2.4; defer to v2.5.

**Rationale:** GSD-2 has no multi-target delivery (renders to disk only). ROUTINES-ADOPTION-NOTES.md §5 designs three targets (local, email, webhook), output truncation, archive schema. The design is sound and aligns with Anvil's vault-only-secrets pattern (webhook URLs from vault). However, v2.4 routines are already ambitious (schedule grammar, pre-dispatch reconciliation, pre-agent script, context chaining, TOML format). Adding email/webhook integration adds: SMTP transport, webhook timeout/retry logic, delivery error surfacing to the UI, output truncation logic, archive schema durability. Estimated 5+ days. **Defer to v2.5** when routine usage is proven and UX feedback clarifies which delivery targets users actually need. For v2.4, local archive only (mandatory per §6 design) and suggest delivery via scripting (a routine can call bash to send email or webhook).

### SKIP: Pre-Agent Script + Wake Gate (Gap 4)

**Recommendation: ADOPT** — implement pre-agent script with wake gate, deferring interpreter plugins.

**Rationale:** This is the highest-leverage feature for cost-conscious routine users. Pattern: routine runs script first, last-line JSON `{"wakeAgent": false}` suppresses LLM invocation (zero cost for "no news" polls). Converts monitoring routines from "always pay" to "pay only when there's news." GSD doesn't have this. ROUTINES-ADOPTION-NOTES.md §3 design is tight: optional `script: PathBuf` on CronEntry, path-confined to `~/.anvil/scripts/`, timeout via config (default 60s), redact secrets in stdout before injection. Implementation: a pre-dispatch hook in the routine executor (new seam before `query()` dispatch). Cost: 2 days (script runner, confinement check, secret redaction, JSON parse). Very high ROI — a routine that polls every 5 minutes for "did the build break" should cost zero until failure appears.

### SKIP: Silent Suppression Marker (Gap 5)

**Recommendation: ADOPT** — implement `[SILENT]` marker as system-prompt addition + one-line check.

**Rationale:** Removes an entire complaint class ("stop spamming me"). Implementation: add to routine system prompt: "If there is genuinely nothing new to report, respond with exactly [SILENT] to suppress delivery." Then in the routine output handler, if response contains `[SILENT]` (case-sensitive), save locally but skip delivery. One line. GSD doesn't have this. Pairs naturally with pre-agent script + wake gate (script can detect no-change and return `{"wakeAgent": false}`, or LLM can voluntarily suppress via `[SILENT]`). Cost: <1 day. Justifies adoption for ergonomics alone.

### ADOPT: Routine Authoring File Format (TOML) (Gap 6)

**Recommendation: ADOPT** — TOML files at `~/.anvil/routines/<name>.toml`.

**Rationale:** GSD-1 uses Markdown+YAML for agents (humanizable but less structured). GSD-2 uses SDK-native specs (not separately documented). Anvil's existing config files (vault scopes, project config) use TOML; adding routine TOML aligns with existing conventions. ROUTINES-ADOPTION-NOTES.md §8 design reuses skill discovery + chains_to machinery for optional `[routine]` block in skills. File format is non-negotiable for production: enables version control, editing, auditing, linting. Cost: 1-2 days (TOML parser, loader, validation, linter for secret patterns like `sk-|ghp_|xoxb-`). Pairs with vault-only-secrets enforcement (config linter warns on raw tokens).

### ADOPT: Context Injection Chaining (Gap 7)

**Recommendation: ADOPT** — linear `context_from` array, defer full DAG to v2.5.

**Rationale:** GSD-1 uses required-reading file refs (works but not composable). GSD-2 derives from session journal (no explicit chaining DSL). Anvil has no context chaining. For v2.4 routines, linear `context_from: ["routine-a", "routine-b"]` covers 80% of use cases ("morning summary that reads three nightly jobs"). Full DAG semantics (parallel fan-out, join rules, `$nodeId.output` substitution) are v2.5+ decisions once usage proves DAG necessity. ROUTINES-ADOPTION-NOTES.md §7 design: routine resolver loads most-recent output of named routines, injects as `## Context From <name>` blocks. Integrates naturally with v2.3 session journal (last output of routine X = last journal entry tagged with X's id). Cost: 1 day (loader, injection, anti-injection delimiters). Essential for multi-routine workflows.

### ADOPT: Token-Budgeted Context Resolution (Gap 8)

**Recommendation: ADOPT** alongside context_from.

**Rationale:** Naive `context_from` injection breaks when three upstream routines exceed the model's context window. GSD-2 session journal tracks tokens but has no token-aware pruning. Anvil has no token-budgeted resolver. ROUTINES-ADOPTION-NOTES.md §7.5 design is elegant: output packets {summary, body, input_hash}, resolver includes all summaries (cheap), allocates bodies by priority (direct dependencies first), truncates from most-distant-upstream when budget exhausted. input_hash enables cache-hit metrics and idempotency. Cost: ~1.5 days (packet schema, resolver algorithm, integration with context_from loader). Very high ROI — prevents silent output loss when context is tight. Pairs with input_hash for free token-economy metrics (count packets where hash matches = "we skipped re-running this routine").

### SKIP: Watch + Webhook + Event Triggers (Gap 9)

**Recommendation: SKIP** for v2.4; design extensibility, ship cron+interval+ISO+one-shot only.

**Rationale:** GSD doesn't have scheduling at all. Anvil ships cron only today. ROUTINES-ADOPTION-NOTES.md §9 identifies three future triggers: watch (filesystem notify via `notify` crate), webhook (inbound HTTPS), event (internal like session.end, git.push, goal.nominated). Design-stage: grammar is extensible (`schedule = "watch:<glob>"`, `schedule = "webhook"`, `schedule = "event:<name>"`), hooks system exists (`crates/runtime/src/hooks.rs`). But implementation deferred to v2.4.x point releases. For v2.4 GA, focus on cron + one-shot (`"in 30m"`) + interval (`"every 5m"`) + ISO timestamp. Cost to defer: zero. Cost to ship all four: 4+ days. Watch needs inotify/FSEvents, webhook needs HTTP listener + HMAC + inbound routing, events need hook wiring through session/push/goal machinery. Ship cron-family triggers in v2.4, design pattern is proven and blocking, defer richer triggers to point releases once cron is stable in production.

---

## Summary of Adopt / Skip decisions

| Gap | Recommendation | Est. Cost | Rationale |
|-----|---|---|---|
| Pre-dispatch reconciliation | ADOPT | 3 days | GSD-2 shipped pattern; directly solves hidden state drift. Essential for reliability. |
| Schedule grammar extensions | ADOPT | 1 day | Low-cost, high ROI for monitoring routines (poll-until-change = zero cost). |
| Multi-target delivery | SKIP | 5+ days deferred | GSD has none; defer to v2.5 post-usage validation. Local archive only for v2.4. |
| Pre-agent script + wake gate | ADOPT | 2 days | Highest-leverage feature for cost-conscious monitoring. Enables zero-cost polling. |
| Silent suppression marker | ADOPT | <1 day | Removes entire complaint class. Pairs with wake gate. |
| Routine TOML format | ADOPT | 1-2 days | Version control, auditing, linting essential for production. Aligns with Anvil conventions. |
| Linear context chaining | ADOPT | 1 day | 80% of use cases; full DAG deferred to v2.5. Pairs with session journal. |
| Token-budgeted context resolution | ADOPT | 1.5 days | Prevents silent output loss. Free token-economy metrics via input_hash. |
| Watch/webhook/event triggers | SKIP (for v2.4) | 4+ days deferred | Design extensibility in grammar, defer richer triggers to v2.4.x after cron stable. |

**Total v2.4 recommended work:** ~11 days (core features); **Total v2.5 lookback:** output delivery, full DAG context, richer triggers.

The pre-dispatch reconciliation (Gap 1) is the load-bearing decision: it's the only GSD-2 pattern unique to this audit, and Anvil will face the same hidden drift problem at scale. Shipping routines without it risks a silent-failure complaint class that won't surface until production. All other adopted features (script, silent marker, context chaining, TOML) are additive ergonomic wins that justify their small cost but are not blockers individually. Delivery, full DAG, and event triggers belong in v2.5 after users establish patterns.
